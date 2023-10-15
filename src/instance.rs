use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use kube::runtime::controller::Action;
use tokio::{spawn, time::timeout};
use tracing::{error_span, info_span};

use crate::{
    background_task,
    crds::Instance,
    event_manager::EventManager,
    ext::ResourceLocalExt,
    mqtt::{ConnectionStatus, Credentials, Manager, Options, Status, Z2MStatus},
    status_manager::StatusManager,
    Context, EmittableResult, EmittedError, Error, Reconciler,
};

#[async_trait]
impl Reconciler for Instance {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, EmittedError> {
        let em = EventManager::new(ctx.client.clone(), self);

        let mut options = Options {
            id: self.full_name(),
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
                    .emit_event_with_path(&em, "spec.credentials.username")
                    .await?,
                password: cred
                    .password
                    .get(&ctx.client, self)
                    .await
                    .emit_event_with_path(&em, "spec.credentials.password")
                    .await?,
            });
        }

        match ctx.state.managers.lock().await.get(&self.full_name()) {
            Some(manager) => match manager.update_options(options.clone()).await {
                Ok(_) => {}
                Err(err) => {
                    info_span!("replacing manager", id = self.full_name(), ?err);
                    ctx.state.managers.lock().await.remove(&self.full_name());
                    manager.close(None).await;
                }
            },
            _ => {}
        }
        if let None = ctx.state.managers.lock().await.get(&self.full_name()) {
            let (manager, mut status_receiver) =
                match timeout(Duration::from_secs(30), Manager::new(options)).await {
                    Ok(result) => result,
                    Err(_) => Err(Error::ActionFailed(
                        "timeout while starting manager instance".to_string(),
                        None,
                    )),
                }
                .emit_event_with_path(&em, "spec")
                .await?;

            spawn(background_task!(
                format!("status reporter for instance {id}", id = self.id()),
                {
                    let eventmanager = EventManager::new(ctx.client.clone(), self);
                    let mut statusmanager = StatusManager::new(ctx.client.clone(), self);
                    async move {
                        statusmanager.update(|s| {
                            s.broker = false;
                        });
                        statusmanager.sync().await;

                        loop {
                            match status_receiver.recv().await {
                                Some(event) => {
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
                                None => {
                                    break;
                                }
                            }
                        }
                        Ok(())
                    }
                },
                {
                    let state = ctx.state.clone();
                    let manager = manager.clone();
                    let name = self.full_name();
                    |err| async move {
                        error_span!("status reporter for instance stopped", ?err);
                        manager.close(Some(Box::new(err))).await;
                        state.managers.lock().await.remove(&name);
                    }
                }
            ));

            ctx.state
                .managers
                .lock()
                .await
                .insert(self.full_name(), manager);
        }

        return Ok(Action::requeue(Duration::from_secs(15)));
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, EmittedError> {
        let manager = ctx.state.managers.lock().await.remove(&self.full_name());
        if let Some(manager) = manager {
            manager.close(None).await;
        }
        return Ok(Action::requeue(Duration::from_secs(5 * 60)));
    }
}
