//! Reconcile logic for [`Group`].

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use kube::{runtime::controller::Action, ResourceExt};

use crate::{
    crds::{Group, GroupStatus},
    error::{EmittableResult, Error},
    event_manager::EventManager,
    mqtt::{BridgeGroup, Manager},
    status_manager::StatusManager,
    Context, EmittedError, Reconciler, TIMEOUT,
};

#[async_trait]
impl Reconciler for Group {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, EmittedError> {
        let eventmanager = EventManager::new(ctx.client.clone(), self);
        let mut statusmanager = StatusManager::new(ctx.client.clone(), self);

        statusmanager.update(|s| {
            s.exists = None;
            s.synced = None;
            s.id = None;
        });

        let manager = self.get_manager(&ctx, &eventmanager).await?;

        statusmanager.update(|s| {
            s.exists = Some(false);
        });

        let group = self.get_or_create_group(&manager, &eventmanager).await?;

        statusmanager.update(|s| {
            s.exists = Some(true);
            s.id = Some(group.id);
            s.synced = Some(false);
        });

        if group.friendly_name != self.spec.friendly_name {
            manager
                .rename_group(group.id, &self.spec.friendly_name)
                .await
                .emit_event_with_path(&eventmanager, "spec.friendly_name")
                .await?;
        }

        statusmanager.update(|s| {
            s.synced = Some(true);
        });

        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, EmittedError> {
        let eventmanager = EventManager::new(ctx.client.clone(), self);
        let manager = self.get_manager(&ctx, &eventmanager).await?;

        if let Some(GroupStatus { id: Some(id), .. }) = self.status {
            manager
                .delete_group(id)
                .await
                .emit_event_with_path(&eventmanager, "status.id")
                .await?;
        }

        return Ok(Action::requeue(Duration::from_secs(5 * 60)));
    }
}
impl Group {
    fn get_id(&self) -> Option<(&str, usize)> {
        let from_status = self
            .status
            .as_ref()
            .and_then(|s| s.id)
            .map(|id| ("status.id", id));
        let from_spec = self.spec.id.map(|id| ("spec.id", id));
        from_status.or(from_spec)
    }

    async fn get_manager(
        &self,
        ctx: &Arc<Context>,
        eventmanager: &EventManager,
    ) -> Result<Arc<Manager>, EmittedError> {
        let instance = format!("{}/{}", self.namespace().unwrap(), self.spec.instance);
        ctx.state
            .managers
            .get(&instance, TIMEOUT)
            .await
            .emit_event_with_path(eventmanager, "spec.instance")
            .await
    }

    async fn get_or_create_group(
        &self,
        manager: &Arc<Manager>,
        eventmanager: &EventManager,
    ) -> Result<BridgeGroup, EmittedError> {
        let mut group: Option<BridgeGroup> = None;
        if let Some((field_path, id)) = self.get_id() {
            group = manager
                .get_group_by_id(id)
                .await
                .emit_event_with_path(eventmanager, field_path)
                .await?;
        }
        if group.is_none() && self.spec.id.is_none() {
            group = manager
                .get_group_by_friendly_name(&self.spec.friendly_name)
                .await
                .emit_event_with_path(eventmanager, "spec.friendly_name")
                .await?;
        }
        if group.is_none() {
            let id = manager
                .create_group(self.spec.id, &self.spec.friendly_name)
                .await
                .emit_event_with_path(eventmanager, "spec")
                .await?;
            group = manager
                .get_group_by_id(id)
                .await
                .emit_event_with_path(eventmanager, "spec.id")
                .await?;
        }
        let Some(group) = group else {
            return Err(Error::ActionFailed(
                "Failed to find or create group.".to_owned(),
                None,
            ))
            .emit_event_with_path(eventmanager, "spec")
            .await;
        };
        Ok(group)
    }
}
