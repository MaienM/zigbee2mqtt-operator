//! Reconcile logic for [`Instance`].

use std::{
    collections::{HashMap, HashSet},
    fmt::{Debug, Display},
    hash::Hash,
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use k8s_openapi::NamespaceResourceScope;
use kube::{api::ListParams, runtime::controller::Action, Api, Client, Resource, ResourceExt};
use serde::Deserialize;
use tokio::{
    spawn,
    sync::{Mutex, RwLock},
    time::timeout,
};
use tracing::{error_span, info_span, warn_span};

use crate::{
    background_task,
    crds::{Device, Group, Instance, InstanceHandleUnmanaged, InstanceStatus, Instanced},
    error::Error,
    event_manager::{EventManager, EventType},
    mqtt::{
        ConnectionStatus, Manager, Options, OptionsCredentials, OptionsTLS, OptionsTLSClient,
        Status, Z2MStatus,
    },
    status_manager::StatusManager,
    with_source::{vws, vws_sub, ValueWithSource},
    Context, ErrorWithMeta, EventCore, ObjectReferenceLocalExt, Reconciler, ResourceLocalExt,
    EXTENSION_CODE, EXTENSION_NAME, RECONCILE_INTERVAL,
};

/// Helper to keep track of the Manager for each instance.
pub(crate) struct InstanceTracker {
    api: Api<Instance>,
    last_updated_at: Mutex<Instant>,
    managers: RwLock<HashMap<String, Option<InstanceTrackerEntry>>>,
}
type InstanceTrackerEntry = (Arc<Manager>, InstanceStatus);
impl InstanceTracker {
    pub fn new(client: Client) -> Arc<Self> {
        Arc::new(Self {
            api: Api::all(client),
            last_updated_at: Mutex::new(
                Instant::now().checked_sub(*RECONCILE_INTERVAL * 2).unwrap(),
            ),
            managers: RwLock::new(HashMap::new()),
        })
    }

    pub async fn get(self: &Arc<Self>, key: &String) -> Result<InstanceTrackStatus, Error> {
        #[allow(clippy::used_underscore_items)]
        Ok(match self._get(key).await {
            InstanceTrackStatus::Missing => {
                if self._update().await? {
                    self._get(key).await
                } else {
                    InstanceTrackStatus::Missing
                }
            }
            status => status,
        })
    }

    async fn _get(self: &Arc<Self>, key: &String) -> InstanceTrackStatus {
        let managers = self.managers.read().await;
        match managers.get(key) {
            Some(Some((manager, status))) => {
                if status.zigbee2mqtt == Some(true) {
                    InstanceTrackStatus::Ready(manager.clone())
                } else {
                    InstanceTrackStatus::Unhealthy
                }
            }
            Some(None) => InstanceTrackStatus::Pending,
            None => InstanceTrackStatus::Missing,
        }
    }

    async fn _update(self: &Arc<Self>) -> Result<bool, Error> {
        let started_at = Instant::now();
        let mut last_updated_at = self.last_updated_at.lock().await;
        if last_updated_at.elapsed() < *RECONCILE_INTERVAL {
            return Ok(started_at < *last_updated_at);
        }

        let instances: HashSet<_> = {
            let lp = ListParams::default();
            let instances = self.api.list_metadata(&lp).await.map_err(|err| {
                Error::ActionFailed(
                    "failed to get list of instances".to_owned(),
                    Some(Arc::new(err)),
                )
            })?;
            instances
                .into_iter()
                .map(|i| i.get_ref().full_name())
                .collect()
        };

        let mut managers = self.managers.write().await;
        let managed: HashSet<_> = managers.keys().cloned().collect();

        for missing in instances.difference(&managed) {
            managers.insert(missing.clone(), None);
        }

        for excess in managed.difference(&instances) {
            if let Some(Some(manager)) = managers.remove(excess) {
                warn_span!(
                    "instance tracker contained manager that doesn't exist in K8s, removed",
                    full_name = excess,
                    ?manager,
                );
            }
        }

        *last_updated_at = Instant::now();

        Ok(true)
    }
}
pub(crate) enum InstanceTrackStatus {
    /// There is no instance with the provided `full_name`.
    Missing,
    /// There is an instance with the provided `full_name`, but there is no manager for it yet.
    Pending,
    /// There is an instance with the provided `full_name`, and there is a manager for it, but the Zigbee2MQTT instance is not healthy.
    Unhealthy,
    /// There is an instance with the provided `full_name`, and there is a manager for it, and the Zigbee2MQTT instance is healthy.
    Ready(Arc<Manager>),
}
macro_rules! get_manager {
    ($self:ident, $ctx:ident) => {{
        let fullname = crate::crds::Instanced::get_instance_fullname($self);
        match $ctx.state.managers.get(&fullname).await? {
            super::instance::InstanceTrackStatus::Missing => {
                return Err(Error::InvalidResource("instance does not exist".to_owned())
                    .caused_by(&fullname));
            }
            status @ (super::instance::InstanceTrackStatus::Pending
            | super::instance::InstanceTrackStatus::Unhealthy) => {
                let problem = match status {
                    super::instance::InstanceTrackStatus::Pending => "not yet ready",
                    super::instance::InstanceTrackStatus::Unhealthy => "not healthy",
                    _ => panic!("this is silly, this shouldn't be possible"),
                };
                tracing::info_span!(
                    "instance used by resource is not usable, postponing reconcile",
                    problem,
                    instance = *fullname,
                    id = crate::ObjectReferenceLocalExt::id(&$self.get_ref()),
                );

                let eventmanager = EventManager::new($ctx.client.clone(), $self.get_ref());
                eventmanager
                    .publish(EventCore {
                        action: "Reconciling".to_string(),
                        note: Some(format!(
                            "instance used by resource is {problem}, postponing reconcile"
                        )),
                        reason: "Created".to_string(),
                        type_: EventType::Warning,
                        field_path: fullname.source().cloned(),
                    })
                    .await;

                return Ok(Action::requeue(*crate::RECONCILE_INTERVAL_FAILURE));
            }
            super::instance::InstanceTrackStatus::Ready(manager) => manager,
        }
    }};
}
pub(crate) use get_manager;

#[async_trait]
impl Reconciler for Instance {
    async fn reconcile(&self, ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta> {
        let eventmanager = EventManager::new(ctx.client.clone(), self.get_ref());

        let manager = self.setup_manager(ctx).await?;
        self.install_extension(&manager).await?;
        self.process_unmanaged::<Device>(ctx, &manager, &eventmanager)
            .await?;
        self.process_unmanaged::<Group>(ctx, &manager, &eventmanager)
            .await?;
        self.restart_if_needed(&manager, &eventmanager).await?;

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }

    async fn cleanup(&self, ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta> {
        let entry = ctx
            .state
            .managers
            .managers
            .write()
            .await
            .remove(&self.get_ref().full_name());
        if let Some(Some((manager, _))) = entry {
            manager.close(None).await;
        }

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }
}
impl Instance {
    async fn to_options(&self, ctx: &Arc<Context>) -> Result<Options, ErrorWithMeta> {
        let mut options = Options {
            client_id: self.get_ref().full_name(),
            host: vws!(self.spec.host),
            port: vws!(self.spec.port),
            tls: ValueWithSource::new(None, Some("spec.tls".to_owned())),
            credentials: ValueWithSource::new(None, Some("spec.credentials".to_owned())),
            base_topic: vws!(self.spec.base_topic),
        };

        if let Some(tls) = vws!(self.spec.tls).transpose() {
            let mut tls_opt = OptionsTLS {
                ca: tls.sub(None, "ca"),
                client: tls.sub(None, "client"),
            };

            if let Some(ca) = vws_sub!(tls.ca).transpose() {
                tls_opt.ca = ca
                    .get(&ctx.client, self)
                    .await?
                    .transform(|v| Some(v.take()));
            }

            if let Some(client) = vws_sub!(tls.client).transpose() {
                tls_opt.client = client.with_value(Some(OptionsTLSClient {
                    cert: vws_sub!(client.cert).get(&ctx.client, self).await?,
                    key: vws_sub!(client.key).get(&ctx.client, self).await?,
                }));
            }

            options.tls = tls.with_value(Some(tls_opt));
        }

        if let Some(cred) = vws!(self.spec.credentials).transpose() {
            options.credentials = cred.with_value(Some(OptionsCredentials {
                username: vws_sub!(cred.username).get(&ctx.client, self).await?,
                password: vws_sub!(cred.password).get(&ctx.client, self).await?,
            }));
        }

        Ok(options)
    }

    #[allow(clippy::too_many_lines)]
    async fn setup_manager(&self, ctx: &Arc<Context>) -> Result<Arc<Manager>, ErrorWithMeta> {
        let mut managers = ctx.state.managers.managers.write().await;
        let full_name = self.get_ref().full_name();
        let entry = managers.entry(full_name.clone()).or_default();

        // Check if manager is connected. If it isn't we discard it just in case it's stuck in some kind of reconnect loop.
        if let Some(status) = &self.status {
            if let Some((manager, _)) = entry {
                if status.zigbee2mqtt != Some(true) {
                    info_span!(
                        "replacing manager",
                        id = self.get_ref().full_name(),
                        reason = "not connected"
                    );
                    manager.close(None).await;
                    *entry = None;
                }
            }
        }

        // Apply options to manager. If this results in an error this means the manager must be recreated so we discard it.
        let options = self.to_options(ctx).await?;
        if let Some((manager, _)) = entry {
            if let Err(err) = manager.update_options(options.clone()).await {
                info_span!("replacing manager", id = self.get_ref().full_name(), ?err);
                manager.close(None).await;
                *entry = None;
            }
        }

        if let Some((manager, _)) = entry {
            return Ok(manager.clone());
        }

        let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
        statusmanager.update(|status| {
            status.broker = false;
            status.zigbee2mqtt = None;
        });
        statusmanager.sync().await;

        let (manager, mut status_receiver) =
            timeout(RECONCILE_INTERVAL.mul_f64(0.8), Manager::new(options))
                .await
                .map_err(|_| {
                    Error::ActionFailed("timeout while starting manager instance".to_string(), None)
                })??;
        let _ = entry.insert((manager.clone(), statusmanager.get().clone()));

        spawn(background_task!(
            format!("status reporter for instance {}", self.full_name()),
            {
                let eventmanager = EventManager::new(ctx.client.clone(), self.get_ref());
                let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
                let managers = ctx.state.managers.clone();
                let full_name = full_name.clone();
                async move {
                    while let Some(event) = status_receiver.recv().await {
                        eventmanager.publish((&event).into()).await;
                        statusmanager.update(|s| {
                            match event {
                                Status::ConnectionStatus(ConnectionStatus::Active) => {
                                    s.broker = true;
                                    s.zigbee2mqtt = Some(false);
                                }
                                Status::ConnectionStatus(
                                    ConnectionStatus::Inactive
                                    | ConnectionStatus::Closed
                                    | ConnectionStatus::Failed(_)
                                    | ConnectionStatus::Refused(_),
                                ) => {
                                    s.broker = false;
                                    s.zigbee2mqtt = None;
                                }
                                Status::ConnectionStatus(_) => {}

                                Status::Z2MStatus(Z2MStatus::HealthOk) => {
                                    s.broker = true;
                                    s.zigbee2mqtt = Some(true);
                                }
                                Status::Z2MStatus(Z2MStatus::HealthError(_)) => {
                                    s.zigbee2mqtt = Some(false);
                                }
                            };
                        });
                        statusmanager.sync().await;
                        if let Some(Some((_, ref mut status))) =
                            managers.managers.write().await.get_mut(&full_name)
                        {
                            *status = statusmanager.get().clone();
                        }
                    }
                    Ok(())
                }
            },
            {
                let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
                let manager = manager.clone();
                let managers = ctx.state.managers.clone();
                |err| async move {
                    error_span!("status reporter for instance stopped", ?err);
                    managers.managers.write().await.remove(&full_name);
                    manager.close(Some(Box::new(err))).await;

                    statusmanager.update(|status| {
                        status.broker = false;
                        status.zigbee2mqtt = None;
                    });
                    statusmanager.sync().await;
                }
            }
        ));

        drop(managers); // ensure lock is held to here

        Ok(manager)
    }

    async fn install_extension(&self, manager: &Arc<Manager>) -> Result<(), ErrorWithMeta> {
        let extensions = manager.get_bridge_extensions_tracker().await?.get().await?;
        let extension = extensions.iter().find(|e| e.name == EXTENSION_NAME);
        if extension.is_none_or(|e| e.code != EXTENSION_CODE) {
            manager
                .install_extension(EXTENSION_NAME, EXTENSION_CODE)
                .await?;
        }

        Ok(())
    }

    async fn process_unmanaged<T>(
        &self,
        ctx: &Arc<Context>,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
    ) -> Result<(), ErrorWithMeta>
    where
        T: ManagedResource,
        <T as ManagedResource>::Resource: Clone + Debug + for<'de> Deserialize<'de>,
        <<T as ManagedResource>::Resource as Resource>::DynamicType: Default,
    {
        let dt = <T::Resource as Resource>::DynamicType::default();

        let mode = T::get_mode(self);
        if mode == &InstanceHandleUnmanaged::Ignore {
            return Ok(());
        }

        let kubernetes_resources: HashSet<_> = {
            let api =
                Api::<T::Resource>::namespaced(ctx.client.clone(), &self.namespace().unwrap());
            let lp = ListParams::default();
            api.list(&lp)
                .await
                .map_err(|err| {
                    Error::ActionFailed(
                        format!("failed to get list of {}", T::Resource::plural(&dt)),
                        Some(Arc::new(err)),
                    )
                })?
                .iter()
                .filter(|r| *r.get_instance_fullname() == self.get_ref().full_name())
                .map(T::kubernetes_resource_identifier)
                .collect()
        };

        let resource_count = kubernetes_resources.len();
        let kubernetes_identifiers: HashSet<_> =
            kubernetes_resources.into_iter().flatten().collect();
        if kubernetes_identifiers.len() < resource_count {
            eventmanager
                .publish(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some(format!(
                        "Some {plural} are still reconciling, so cannot check for unmanaged resources.",
                        plural = T::Resource::plural(&dt),
                    )),
                    reason: "Created".to_string(),
                    type_: EventType::Warning,
                    field_path: None,
                })
                .await;
            return Ok(());
        }

        let zigbee2mqtt_resources = T::get_zigbee2mqtt(manager).await?;

        for (id, name) in zigbee2mqtt_resources {
            if kubernetes_identifiers.contains(&id) {
                continue;
            }

            eventmanager
                .publish(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some(format!(
                        "Zigbee2MQTT has {kind} {id} (name '{name}') which is not defined in K8s{extra}.",
                        kind = T::Resource::kind(&dt),
                        extra = if mode == &InstanceHandleUnmanaged::Delete {
                            ", deleting"
                        } else {
                            ""
                        },
                    )),
                    reason: "Created".to_string(),
                    type_: EventType::Warning,
                    field_path: None,
                })
                .await;

            if mode == &InstanceHandleUnmanaged::Delete {
                T::delete_zigbee2mqtt(manager, id).await?;
            }
        }

        Ok(())
    }

    async fn restart_if_needed(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
    ) -> Result<(), ErrorWithMeta> {
        let restart_required = manager
            .get_bridge_info_tracker()
            .await?
            .get()
            .await?
            .restart_required;
        if restart_required {
            eventmanager
                .publish(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some("Zigbee2MQTT requested restart, doing so now. Any reconcile actions on this instance during this restart will fail.".to_owned()),
                    reason: "Created".to_string(),
                    type_: EventType::Normal,
                    field_path: None,
                })
            .await;
            manager.restart_zigbee2mqtt().await?;
        }
        Ok(())
    }
}

#[async_trait]
trait ManagedResource {
    type Resource: Resource<Scope = NamespaceResourceScope> + Instanced;
    type Identifier: PartialEq + Eq + Hash + Debug + Display + Send;

    /// Get the management mode for the resource for the given instance.
    fn get_mode(instance: &Instance) -> &InstanceHandleUnmanaged;

    /// Get the identifier of a managed Kubernetes resource.
    fn kubernetes_resource_identifier(resource: &Self::Resource) -> Option<Self::Identifier>;

    /// Get the identifiers & names of the existing Zigbee2MQTT resources.
    async fn get_zigbee2mqtt(
        manager: &Arc<Manager>,
    ) -> Result<Vec<(Self::Identifier, String)>, Error>;

    /// Delete the Zigbee2MQTT instance with the given identifier.
    async fn delete_zigbee2mqtt(
        manager: &Arc<Manager>,
        id: Self::Identifier,
    ) -> Result<(), ErrorWithMeta>;
}

#[async_trait]
impl ManagedResource for Device {
    type Resource = Self;
    type Identifier = String;

    fn get_mode(instance: &Instance) -> &InstanceHandleUnmanaged {
        &instance.spec.unmanaged_devices
    }

    fn kubernetes_resource_identifier(resource: &Self::Resource) -> Option<Self::Identifier> {
        Some(resource.spec.ieee_address.clone())
    }

    async fn get_zigbee2mqtt(
        manager: &Arc<Manager>,
    ) -> Result<Vec<(Self::Identifier, String)>, Error> {
        Ok(manager
            .get_bridge_device_tracker()
            .await?
            .get_all()
            .await?
            .into_iter()
            .map(|d| (d.ieee_address, d.friendly_name))
            .collect())
    }

    async fn delete_zigbee2mqtt(
        _manager: &Arc<Manager>,
        _id: Self::Identifier,
    ) -> Result<(), ErrorWithMeta> {
        Err(Error::ActionFailed(
            "deleting of unmanaged devices in Zigbee2MQTT is not supported".to_owned(),
            None,
        ))?
    }
}

#[async_trait]
impl ManagedResource for Group {
    type Resource = Self;
    type Identifier = usize;

    fn get_mode(instance: &Instance) -> &InstanceHandleUnmanaged {
        &instance.spec.unmanaged_groups
    }

    fn kubernetes_resource_identifier(resource: &Self::Resource) -> Option<Self::Identifier> {
        resource.status.as_ref().and_then(|s| s.id)
    }

    async fn get_zigbee2mqtt(
        manager: &Arc<Manager>,
    ) -> Result<Vec<(Self::Identifier, String)>, Error> {
        Ok(manager
            .get_bridge_group_tracker()
            .await?
            .get_all()
            .await?
            .into_iter()
            .map(|d| (d.id, d.friendly_name))
            .collect())
    }

    async fn delete_zigbee2mqtt(
        manager: &Arc<Manager>,
        id: Self::Identifier,
    ) -> Result<(), ErrorWithMeta> {
        manager.delete_group(id.into()).await
    }
}
