//! Reconcile logic for [`Device`].

use std::sync::Arc;

use async_trait::async_trait;
use kube::runtime::controller::Action;

use super::instance::get_manager;
use crate::{
    crds::Device,
    error::Error,
    event_manager::{EventCore, EventManager, EventType},
    mqtt::Manager,
    status_manager::StatusManager,
    with_source::{vws, ValueWithSource},
    Context, ErrorWithMeta, Reconciler, ResourceLocalExt, RECONCILE_INTERVAL,
};

#[async_trait]
impl Reconciler for Device {
    async fn reconcile(&self, ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta> {
        let eventmanager = EventManager::new(ctx.client.clone(), self.get_ref());
        let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
        statusmanager.update(|s| {
            s.exists = None;
            s.synced = None;
        });

        let manager = get_manager!(self, ctx);

        statusmanager.update(|s| {
            s.exists = Some(false);
        });

        let wanted_friendly_name = self.sync_name(&manager, &eventmanager).await?;

        statusmanager.update(|s| {
            s.exists = Some(true);
            s.synced = Some(false);
        });

        self.sync_options(&manager, &eventmanager).await?;
        self.sync_capabilities(wanted_friendly_name, &manager, &eventmanager)
            .await?;

        statusmanager.update(|s| {
            s.synced = Some(true);
        });

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }

    async fn cleanup(&self, _ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta> {
        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }
}
impl Device {
    async fn sync_name(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
    ) -> Result<ValueWithSource<String>, ErrorWithMeta> {
        let ieee_address = vws!(self.spec.ieee_address);
        let wanted_friendly_name = vws!(self.spec.friendly_name)
            .transpose()
            .unwrap_or(vws!(self.spec.ieee_address));
        let info = manager
            .get_bridge_device_tracker()
            .await
            .map_err(Error::unknown_cause)?
            .get_device(&self.spec.ieee_address)
            .await
            .map_err(|err| err.caused_by(&ieee_address))?;
        if info.friendly_name != *wanted_friendly_name {
            eventmanager
                .publish(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some(format!(
                        "updating friendly name to {wanted_friendly_name} (previously {old_friendly_name})",
                        old_friendly_name = info.friendly_name,
                    )),
                    reason: "Created".to_string(),
                    type_: EventType::Normal,
                    ..EventCore::default()
                })
                .await;
            manager
                .rename_device(ieee_address, wanted_friendly_name.clone())
                .await?;
        }
        Ok(wanted_friendly_name)
    }

    async fn sync_options(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
    ) -> Result<(), ErrorWithMeta> {
        let Some(ref wanted_options) = vws!(self.spec.options).transpose() else {
            return Ok(());
        };

        let mut options_manager = manager
            .get_device_options_manager(vws!(self.spec.ieee_address))
            .await?;
        options_manager
            .sync(eventmanager, wanted_options.clone().values())
            .await
    }

    async fn sync_capabilities(
        &self,
        friendly_name: ValueWithSource<String>,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
    ) -> Result<(), ErrorWithMeta> {
        let Some(ref wanted_capabilities) = vws!(self.spec.capabilities).transpose() else {
            return Ok(());
        };

        let mut capabilities_manager = manager
            .get_device_capabilities_manager(vws!(self.spec.ieee_address), friendly_name)
            .await?;
        capabilities_manager
            .sync(eventmanager, wanted_capabilities.clone().values())
            .await
    }
}
