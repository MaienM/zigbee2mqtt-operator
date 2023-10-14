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
    mqtt::{MQTTCredentials, MQTTManager, MQTTOptions, MQTTStatus},
    status_manager::StatusManager,
    Context, Error, Reconciler,
};

#[async_trait]
impl Reconciler for Instance {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, Error> {
        let mut options = MQTTOptions {
            id: self.full_name(),
            host: self.spec.host.clone(),
            port: self.spec.port,
            credentials: None,
        };

        if let Some(cred) = &self.spec.credentials {
            options.credentials = Some(MQTTCredentials {
                username: cred.username.clone(),
                password: cred.password.clone(),
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
                match timeout(Duration::from_secs(30), MQTTManager::new(options)).await {
                    Ok(result) => result,
                    Err(_) => Err(Error::ActionFailed(
                        "timeout while starting manager instance".to_string(),
                        None,
                    )),
                }?;

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
                                            MQTTStatus::Connected => {
                                                s.broker = true;
                                            }
                                            MQTTStatus::Stopped
                                            | MQTTStatus::ConnectionClosed
                                            | MQTTStatus::ConnectionFailed(_)
                                            | MQTTStatus::ConnectionRefused(_) => {
                                                s.broker = false;
                                            }
                                            _ => {}
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

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, Error> {
        let manager = ctx.state.managers.lock().await.remove(&self.full_name());
        if let Some(manager) = manager {
            manager.close(None).await;
        }
        return Ok(Action::requeue(Duration::from_secs(5 * 60)));
    }
}
