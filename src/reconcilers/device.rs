//! Reconcile logic for [`Device`].

use std::sync::Arc;

use async_trait::async_trait;
use kube::{runtime::controller::Action, ResourceExt};

use crate::{
    crds::Device,
    error::EmittableResultFuture,
    event_manager::{EventCore, EventManager, EventType},
    mqtt::Manager,
    status_manager::StatusManager,
    Context, EmittedError, Reconciler, RECONCILE_INTERVAL, TIMEOUT,
};

#[async_trait]
impl Reconciler for Device {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, EmittedError> {
        let mut eventmanager = EventManager::new(ctx.client.clone(), self);
        let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
        statusmanager.update(|s| {
            s.exists = None;
            s.synced = None;
        });

        let instance = format!("{}/{}", self.namespace().unwrap(), self.spec.instance);
        let manager = ctx
            .state
            .managers
            .get(&instance, *TIMEOUT)
            .emit_event(&eventmanager, "spec.instance")
            .await?;

        statusmanager.update(|s| {
            s.exists = Some(false);
        });

        let wanted_friendly_name = self.sync_name(&manager, &mut eventmanager).await?;

        statusmanager.update(|s| {
            s.exists = Some(true);
            s.synced = Some(false);
        });

        self.sync_options(&manager, &mut eventmanager).await?;
        self.sync_capabilities(wanted_friendly_name, &manager, &mut eventmanager)
            .await?;

        statusmanager.update(|s| {
            s.synced = Some(true);
        });

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }

    async fn cleanup(&self, _ctx: Arc<Context>) -> Result<Action, EmittedError> {
        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }
}
impl Device {
    async fn sync_name(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &mut EventManager,
    ) -> Result<String, EmittedError> {
        let wanted_friendly_name = self
            .spec
            .friendly_name
            .clone()
            .unwrap_or_else(|| self.spec.ieee_address.clone());
        let info = manager
            .get_bridge_device_tracker()
            .emit_event(eventmanager, "spec.instance")
            .await?
            .get_device(&self.spec.ieee_address)
            .emit_event(eventmanager, "spec.ieee_address")
            .await?;
        if info.friendly_name != wanted_friendly_name {
            eventmanager
                    .publish(EventCore {
                        action: "Reconciling".to_string(),
                        note: Some(format!(
                            "updating friendly name to {wanted_friendly_name} (previously {old_friendly_name})",
                            old_friendly_name=info.friendly_name,
                        )),
                        reason: "Created".to_string(),
                        type_: EventType::Normal,
                        ..EventCore::default()
                    })
                    .await;
            manager
                .rename_device(&self.spec.ieee_address, &wanted_friendly_name)
                .emit_event(eventmanager, "spec.friendly_name")
                .await?;
        }
        Ok(wanted_friendly_name)
    }

    async fn sync_options(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &mut EventManager,
    ) -> Result<(), EmittedError> {
        let Some(ref wanted_options) = self.spec.options else {
            return Ok(());
        };

        let mut options_manager = manager
            .get_device_options_manager(self.spec.ieee_address.clone())
            .emit_event(eventmanager, "spec.ieee_address")
            .await?;
        options_manager
            .sync(eventmanager, wanted_options.clone())
            .await
    }

    async fn sync_capabilities(
        &self,
        friendly_name: String,
        manager: &Arc<Manager>,
        eventmanager: &mut EventManager,
    ) -> Result<(), EmittedError> {
        let Some(ref wanted_capabilities) = self.spec.capabilities else {
            return Ok(());
        };

        let mut capabilities_manager = manager
            .get_device_capabilities_manager(self.spec.ieee_address.clone(), friendly_name)
            .emit_event(eventmanager, "spec.friendly_name")
            .await?;
        capabilities_manager
            .sync(eventmanager, wanted_capabilities.clone())
            .await
    }
}
