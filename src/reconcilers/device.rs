//! Reconcile logic for [`Device`].

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::Future;
use kube::{runtime::controller::Action, ResourceExt};
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::{
    crds::Device,
    error::{EmittableResult, Error},
    event_manager::{EventCore, EventManager, EventType},
    mqtt::Manager,
    status_manager::StatusManager,
    Context, EmittedError, Reconciler, TIMEOUT,
};

struct Difference {
    key: String,
    wanted: String,
    actual: String,
}
fn find_differences(wanted: &Map<String, Value>, actual: &Map<String, Value>) -> Vec<Difference> {
    let mut differences = Vec::new();
    for key in wanted.keys() {
        let (wanted, actual) = match (wanted.get(key), actual.get(key)) {
            (None | Some(Value::Null), None | Some(Value::Null)) => {
                continue;
            }
            (None | Some(Value::Null), Some(actual)) => (
                "default value".to_string(),
                serde_json::to_string(actual).unwrap_or_else(|_| actual.to_string()),
            ),
            (Some(wanted), None | Some(Value::Null)) => (
                serde_json::to_string(wanted).unwrap_or_else(|_| wanted.to_string()),
                "default value".to_string(),
            ),
            (Some(wanted), Some(actual)) => {
                if wanted == actual {
                    continue;
                }
                (
                    serde_json::to_string(wanted).unwrap_or_else(|_| wanted.to_string()),
                    serde_json::to_string(actual).unwrap_or_else(|_| actual.to_string()),
                )
            }
        };
        differences.push(Difference {
            key: key.clone(),
            wanted,
            actual,
        });
    }
    differences
}

async fn do_sync<F>(
    eventmanager: &mut EventManager,
    name: &str,
    field_path: &str,
    wanted: &Map<String, Value>,
    actual: &Map<String, Value>,
    mut apply: impl FnMut() -> F,
) -> Result<(), EmittedError>
where
    F: Future<Output = Result<Map<String, Value>, Error>>,
{
    let differences = find_differences(wanted, actual);
    for difference in &differences {
        eventmanager
            .publish(EventCore {
                action: "Reconciling".to_string(),
                note: Some(format!(
                    "setting {name} {key} to {wanted} (previously {actual})",
                    key = difference.key,
                    wanted = difference.wanted,
                    actual = difference.actual,
                )),
                reason: "Created".to_string(),
                type_: EventType::Normal,
                field_path: Some(format!("{}.{}", field_path, difference.key)),
            })
            .await;
    }
    if differences.is_empty() {
        return Ok(());
    }

    let result = apply()
        .await
        .emit_event_with_path(eventmanager, field_path)
        .await?;
    let differences = find_differences(wanted, &result);
    for difference in &differences {
        let Difference {
            key,
            wanted,
            actual,
        } = difference;
        eventmanager
            .publish(EventCore {
                action: "Reconciling".to_string(),
                note: Some(format!(
                    "unexpected result of setting {name} {key}, expected {wanted} but got {actual}",
                )),
                reason: "Created".to_string(),
                type_: EventType::Warning,
                field_path: Some(format!("{field_path}.{key}")),
            })
            .await;
    }
    if !differences.is_empty() {
        return Err(Error::InvalidResource {
            field_path: String::new(),
            message: format!("failed to set at least one {name}"),
        })
        .fake_emit_event();
    }
    Ok(())
}

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
            .get(&instance, TIMEOUT)
            .await
            .emit_event_with_path(&eventmanager, "spec.instance")
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

        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    async fn cleanup(&self, _ctx: Arc<Context>) -> Result<Action, EmittedError> {
        return Ok(Action::requeue(Duration::from_secs(5 * 60)));
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
            .get_bridge_device_definition(&self.spec.ieee_address)
            .await
            .emit_event_with_path(eventmanager, "spec.ieee_address")
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
                .await
                .emit_event_with_path(eventmanager, "spec.friendly_name")
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
            .await
            .emit_event_with_path(eventmanager, "spec.ieee_address")
            .await?;
        let current_options = options_manager
            .get()
            .await
            .emit_event_with_path(eventmanager, "spec.ieee_address")
            .await?;

        // Set all options that are currently set but which are not present in the spec to null as this will restore them to their default values.
        let mut wanted_options = wanted_options.clone();
        for key in current_options.keys() {
            if key == "friendly_name" {
                continue;
            }
            wanted_options.entry(key.clone()).or_insert(Value::Null);
        }

        let options_manager = Arc::new(Mutex::new(options_manager));
        do_sync(
            eventmanager,
            "option",
            "spec.options",
            &wanted_options,
            &current_options,
            || {
                let options_manager = options_manager.clone();
                let wanted_options = wanted_options.clone();
                async move {
                    let mut options_manager = options_manager.lock().await;
                    options_manager.set(&wanted_options).await
                }
            },
        )
        .await?;
        Ok(())
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
            .await
            .emit_event_with_path(eventmanager, "spec.friendly_name")
            .await?;
        let current_capabilities = capabilities_manager
            .get()
            .await
            .emit_event_with_path(eventmanager, "spec.friendly_name")
            .await?;

        let capabilities_manager = Arc::new(Mutex::new(capabilities_manager));
        do_sync(
            eventmanager,
            "capability",
            "spec.capabilities",
            wanted_capabilities,
            &current_capabilities,
            || {
                let capabilities_manager = capabilities_manager.clone();
                let wanted_capabilities = wanted_capabilities.clone();
                async move {
                    let mut capabilities_manager = capabilities_manager.lock().await;
                    capabilities_manager.set(wanted_capabilities.into()).await
                }
            },
        )
        .await?;
        Ok(())
    }
}
