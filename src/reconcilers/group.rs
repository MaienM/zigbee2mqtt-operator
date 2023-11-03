//! Reconcile logic for [`Group`].

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use kube::runtime::controller::Action;

use super::instance::get_manager;
use crate::{
    crds::Group,
    error::Error,
    event_manager::{EventManager, EventType},
    mqtt::{BridgeGroup, Manager},
    status_manager::StatusManager,
    with_source::{vws, vws_sub, ValueWithSource},
    Context, ErrorWithMeta, EventCore, Reconciler, ResourceLocalExt, RECONCILE_INTERVAL,
};

struct BridgeGroupInfo {
    pub id: ValueWithSource<usize>,
    pub info: BridgeGroup,
}

#[async_trait]
impl Reconciler for Group {
    async fn reconcile(&self, ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta> {
        let eventmanager = EventManager::new(ctx.client.clone(), self.get_ref());
        let mut statusmanager = StatusManager::new(ctx.client.clone(), self);

        statusmanager.update(|s| {
            s.exists = None;
            s.synced = None;
            s.id = None;
        });

        let manager = get_manager!(self, ctx);

        statusmanager.update(|s| {
            s.exists = Some(false);
        });

        let group = self.sync_existence(&manager, &eventmanager).await?;

        statusmanager.update(|s| {
            s.exists = Some(true);
            s.id = Some(*group.id);
            s.synced = Some(false);
        });

        self.sync_name(&manager, &group).await?;
        self.sync_members(&manager, &eventmanager, &group).await?;
        self.sync_options(&manager, &eventmanager, &group).await?;

        statusmanager.update(|s| {
            s.synced = Some(true);
        });

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }

    async fn cleanup(&self, ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta> {
        let manager = get_manager!(self, ctx);

        if let Some(status) = vws!(self.status).transpose() {
            if let Some(id) = vws_sub!(status.id).transpose() {
                manager.delete_group(id).await?;
            }
        }

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }
}
impl Group {
    fn get_id(&self) -> Option<ValueWithSource<usize>> {
        let from_status = self
            .status
            .as_ref()
            .and_then(|s| s.id)
            .map(|id| ValueWithSource::new(id, Some("status.id".to_owned())));
        let from_spec = vws!(self.spec.id).transpose();
        from_status.or(from_spec)
    }

    async fn get_or_create_group(
        &self,
        manager: &Arc<Manager>,
    ) -> Result<BridgeGroupInfo, ErrorWithMeta> {
        let mut group: Option<BridgeGroupInfo> = None;
        let tracker = manager.get_bridge_group_tracker().await?;
        if let Some(id) = self.get_id() {
            group = tracker
                .get_by_id(*id)
                .await?
                .map(|info| BridgeGroupInfo { id, info });
        }
        if group.is_none() && self.spec.id.is_none() {
            group = tracker
                .get_by_friendly_name(&self.spec.friendly_name)
                .await?
                .map(|info| BridgeGroupInfo {
                    id: ValueWithSource::new(info.id, Some("status.id".to_owned())),
                    info,
                });
        }
        if group.is_none() {
            let id = manager
                .create_group(vws!(self.spec.id), vws!(self.spec.friendly_name))
                .await?;
            group = tracker.get_by_id(id).await?.map(|info| BridgeGroupInfo {
                id: ValueWithSource::new(info.id, Some("status.id".to_owned())),
                info,
            });
        }
        let Some(group) = group else {
            return Err(Error::ActionFailed(
                "failed to find or create group".to_owned(),
                None,
            ))?;
        };
        Ok(group)
    }

    async fn sync_existence(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
    ) -> Result<BridgeGroupInfo, ErrorWithMeta> {
        let mut group = self.get_or_create_group(manager).await?;
        if let Some(id) = vws!(self.spec.id).transpose() {
            if *id != *group.id {
                eventmanager
                    .publish(EventCore {
                        action: "Reconciling".to_string(),
                        note: Some("id changed, deleting and recreating group".to_owned()),
                        reason: "Changed".to_string(),
                        type_: EventType::Normal,
                        field_path: Some("spec.id".to_owned()),
                    })
                    .await;
                manager.delete_group(group.id).await?;
                group = self.get_or_create_group(manager).await?;
            }
        }
        Ok(group)
    }

    async fn sync_name(
        &self,
        manager: &Arc<Manager>,
        group: &BridgeGroupInfo,
    ) -> Result<(), ErrorWithMeta> {
        let friendly_name = vws!(self.spec.friendly_name);
        if group.info.friendly_name != *friendly_name {
            manager
                .rename_group(group.id.clone(), friendly_name)
                .await?;
        }
        Ok(())
    }

    async fn sync_members(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
        group: &BridgeGroupInfo,
    ) -> Result<(), ErrorWithMeta> {
        let Some(ref members) = vws!(self.spec.members).transpose() else {
            return Ok(());
        };

        let devices: HashMap<String, String> = manager
            .get_bridge_device_tracker()
            .await?
            .get_all()
            .await?
            .into_iter()
            .flat_map(|d| {
                [
                    (d.ieee_address.clone(), d.ieee_address.clone()),
                    (d.friendly_name, d.ieee_address),
                ]
            })
            .collect();

        let mut wanted = HashSet::new();
        for member in members {
            match devices.get(*member) {
                Some(address) => {
                    wanted.insert(member.with_value(address.clone()));
                }
                None => {
                    return Err(Error::ActionFailed(
                        format!("cannot find device with ieee_address or friendly_name '{member}'"),
                        None,
                    )
                    .caused_by(&member));
                }
            }
        }

        let current: HashSet<ValueWithSource<_>> = group
            .info
            .members
            .iter()
            .map(|m| m.ieee_address.clone().into())
            .collect();

        for address in current.difference(&wanted) {
            eventmanager
                .publish(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some(format!("removing device {address} from group")),
                    reason: "Changed".to_string(),
                    type_: EventType::Normal,
                    field_path: Some("spec.members".to_owned()),
                })
                .await;
            manager
                .remove_group_member(group.id.clone(), address.clone())
                .await?;
        }

        for address in wanted.difference(&current) {
            eventmanager
                .publish(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some(format!("adding device {address} to group")),
                    reason: "Changed".to_string(),
                    type_: EventType::Normal,
                    field_path: address.source().cloned(),
                })
                .await;
            manager
                .add_group_member(group.id.clone(), address.clone())
                .await?;
        }

        Ok(())
    }

    async fn sync_options(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
        group: &BridgeGroupInfo,
    ) -> Result<(), ErrorWithMeta> {
        let Some(ref wanted_options) = vws!(self.spec.options).transpose() else {
            return Ok(());
        };

        let mut options_manager = manager.get_group_options_manager(group.id.clone()).await?;
        options_manager
            .sync(eventmanager, wanted_options.clone())
            .await
    }
}
