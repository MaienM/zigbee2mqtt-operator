//! Reconcile logic for [`Instance`].

use std::{
    collections::HashSet,
    fmt::{Debug, Display},
    hash::Hash,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use k8s_openapi::NamespaceResourceScope;
use kube::{api::ListParams, runtime::controller::Action, Api, Resource, ResourceExt};
use serde::Deserialize;
use tokio::{spawn, time::timeout};
use tracing::{error_span, info_span};

use crate::{
    background_task,
    crds::{Device, Group, Instance, InstanceHandleUnmanaged, Instanced},
    error::Error,
    event_manager::{EventManager, EventType},
    mqtt::{ConnectionStatus, Credentials, Manager, Options, Status, Z2MStatus},
    status_manager::StatusManager,
    with_source::{vws, vws_sub},
    Context, ErrorWithMeta, EventCore, ObjectReferenceLocalExt, Reconciler, ResourceLocalExt,
    RECONCILE_INTERVAL,
};

#[async_trait]
impl Reconciler for Instance {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, ErrorWithMeta> {
        let eventmanager = EventManager::new(ctx.client.clone(), self.get_ref());

        let manager = self.setup_manager(&ctx).await?;
        self.process_unmanaged::<Device>(&ctx, &manager, &eventmanager)
            .await?;
        self.process_unmanaged::<Group>(&ctx, &manager, &eventmanager)
            .await?;
        self.restart_if_needed(&manager, &eventmanager).await?;

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, ErrorWithMeta> {
        let manager = ctx.state.managers.remove(&self.get_ref().full_name()).await;
        if let Some(manager) = manager {
            if let Ok(manager) = manager.get(Duration::from_secs(60)).await {
                manager.close(None).await;
            }
            // No else clause because if getting the manager timed out this must mean that it was never created successfully, so there is nothing to close/cleanup.
        }

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }
}
impl Instance {
    async fn to_options(&self, ctx: &Arc<Context>) -> Result<Options, ErrorWithMeta> {
        let mut options = Options {
            client_id: self.get_ref().full_name(),
            host: self.spec.host.clone(),
            port: self.spec.port,
            credentials: None,
            base_topic: self.spec.base_topic.clone(),
        };

        if let Some(cred) = vws!(self.spec.credentials).transpose() {
            options.credentials = Some(Credentials {
                username: vws_sub!(cred.username).get(&ctx.client, self).await?.take(),
                password: vws_sub!(cred.password).get(&ctx.client, self).await?.take(),
            });
        }

        Ok(options)
    }

    async fn setup_manager(&self, ctx: &Arc<Context>) -> Result<Arc<Manager>, ErrorWithMeta> {
        let manager_awaitable = ctx
            .state
            .managers
            .get_or_create(self.get_ref().full_name())
            .await;

        // Apply options to manager. If this results in an error this means the manager must be recreated so we discard it.
        let options = self.to_options(ctx).await?;
        let manager = match manager_awaitable.get_immediate().await {
            Some(Ok(manager)) => match manager.update_options(options.clone()).await {
                Ok(_) => Some(manager),
                Err(err) => {
                    info_span!("replacing manager", id = self.get_ref().full_name(), ?err);
                    manager_awaitable.clear().await;
                    manager.close(None).await;
                    None
                }
            },
            _ => None,
        };

        if let Some(manager) = manager {
            return Ok(manager);
        }

        let (manager, mut status_receiver) =
            timeout(Duration::from_secs(30), Manager::new(options))
                .await
                .map_err(|_| {
                    Error::ActionFailed("timeout while starting manager instance".to_string(), None)
                })??;

        spawn(background_task!(
            format!("status reporter for instance {}", self.full_name()),
            {
                let eventmanager = EventManager::new(ctx.client.clone(), self.get_ref());
                let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
                let lock = manager_awaitable.clone();
                async move {
                    let _lock = lock; // Keep a reference to the manager's AwaitableValue as long as it is in use.

                    statusmanager.update(|s| {
                        s.broker = false;
                        s.zigbee2mqtt = false;
                    });
                    statusmanager.sync().await;

                    while let Some(event) = status_receiver.recv().await {
                        eventmanager.publish((&event).into()).await;
                        statusmanager.update(|s| {
                            match event {
                                Status::ConnectionStatus(ConnectionStatus::Active) => {
                                    s.broker = true;
                                }
                                Status::ConnectionStatus(
                                    ConnectionStatus::Inactive
                                    | ConnectionStatus::Closed
                                    | ConnectionStatus::Failed(_)
                                    | ConnectionStatus::Refused(_),
                                ) => {
                                    s.broker = false;
                                    s.zigbee2mqtt = false;
                                }
                                Status::ConnectionStatus(_) => {}

                                Status::Z2MStatus(Z2MStatus::HealthOk) => {
                                    s.broker = true;
                                    s.zigbee2mqtt = true;
                                }
                                Status::Z2MStatus(Z2MStatus::HealthError(_)) => {
                                    s.zigbee2mqtt = false;
                                }
                            };
                        });
                        statusmanager.sync().await;
                    }
                    Ok(())
                }
            },
            {
                let manager_awaitable = manager_awaitable.clone();
                let manager = manager.clone();
                |err| async move {
                    error_span!("status reporter for instance stopped", ?err);
                    manager_awaitable.clear().await;
                    let shutdown_reason = manager.close(Some(Box::new(err))).await;
                    manager_awaitable.invalidate(shutdown_reason).await;
                }
            }
        ));
        manager_awaitable.set(manager.clone()).await;

        Ok(manager)
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

        let kubernetes_identifiers = {
            let api =
                Api::<T::Resource>::namespaced(ctx.client.clone(), &self.namespace().unwrap());
            let lp = ListParams::default();
            api.list(&lp)
                .await
                .map_err(|err| {
                    Error::ActionFailed(
                        format!("failed to get list of {}", T::Resource::plural(&dt)),
                        Some(Arc::new(Box::new(err))),
                    )
                })?
                .iter()
                .filter(|r| *r.get_instance_fullname() == self.get_ref().full_name())
                .filter_map(T::kubernetes_resource_identifier)
                .collect::<HashSet<_>>()
        };

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
    type Identifier: PartialEq + Eq + Hash + Display + Send;

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
