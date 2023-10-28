//! Reconcile logic for [`Instance`].

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use kube::runtime::controller::Action;
use tokio::{spawn, time::timeout};
use tracing::{error_span, info_span};

use crate::{
    background_task,
    crds::Instance,
    error::{EmittableResult, Error},
    event_manager::EventManager,
    mqtt::{ConnectionStatus, Credentials, Manager, Options, Status, Z2MStatus},
    status_manager::StatusManager,
    Context, EmittedError, Reconciler, ResourceLocalExt, RECONCILE_INTERVAL,
};

#[async_trait]
impl Reconciler for Instance {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, EmittedError> {
        let manager_awaitable = ctx.state.managers.get_or_create(self.full_name()).await;
        let eventmanager = EventManager::new(ctx.client.clone(), self);

        // Apply options to manager. If this results in an error this means the manager must be recreated so we discard it.
        let options = self.to_options(&ctx, &eventmanager).await?;
        let manager = match manager_awaitable.get_immediate().await {
            Some(Ok(manager)) => match manager.update_options(options.clone()).await {
                Ok(_) => Some(manager),
                Err(err) => {
                    info_span!("replacing manager", id = self.full_name(), ?err);
                    manager_awaitable.clear().await;
                    manager.close(None).await;
                    None
                }
            },
            _ => None,
        };

        if manager.is_none() {
            let (manager, mut status_receiver) =
                match timeout(Duration::from_secs(30), Manager::new(options)).await {
                    Ok(result) => result,
                    Err(_) => Err(Error::ActionFailed(
                        "timeout while starting manager instance".to_string(),
                        None,
                    )),
                }
                .emit_event_with_path(&eventmanager, "spec")
                .await?;

            spawn(background_task!(
                format!("status reporter for instance {id}", id = self.id()),
                {
                    let eventmanager = EventManager::new(ctx.client.clone(), self);
                    let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
                    let _lock = manager_awaitable.clone();
                    async move {
                        let _lock = _lock; // Keep a reference to the manager's AwaitableValue as long as it is in use.

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
            manager_awaitable.set(manager).await;
        };

        Ok(Action::requeue(*RECONCILE_INTERVAL))
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, EmittedError> {
        let manager = ctx.state.managers.remove(&self.full_name()).await;
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
    async fn to_options(
        &self,
        ctx: &Arc<Context>,
        eventmanager: &EventManager,
    ) -> Result<Options, EmittedError> {
        let mut options = Options {
            client_id: self.full_name(),
            host: self.spec.host.clone(),
            port: self.spec.port,
            credentials: None,
            base_topic: self.spec.base_topic.clone(),
        };

        if let Some(cred) = &self.spec.credentials {
            options.credentials = Some(Credentials {
                username: cred
                    .username
                    .get(&ctx.client, self)
                    .await
                    .emit_event_with_path(eventmanager, "spec.credentials.username")
                    .await?,
                password: cred
                    .password
                    .get(&ctx.client, self)
                    .await
                    .emit_event_with_path(eventmanager, "spec.credentials.password")
                    .await?,
            });
        }

        Ok(options)
    }
}
