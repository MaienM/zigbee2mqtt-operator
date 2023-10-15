use std::{collections::HashMap, str::from_utf8, sync::Arc};

use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client, CustomResource, Resource, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{ext::ResourceLocalExt, Error};

///
/// A value that can be indirect, like environment variables.
///
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub enum Value {
    #[serde(rename = "value")]
    Inline(String),
    #[serde(rename = "valueFrom", rename_all = "camelCase")]
    Secret { secret_key_ref: SecretKeyRef },
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct SecretKeyRef {
    namespace: Option<String>,
    name: String,
    key: String,
}
impl Value {
    /// Get the current value.
    pub async fn get<T>(&self, client: &Client, resource: &T) -> Result<String, Error>
    where
        T: Resource + ResourceLocalExt,
    {
        return match self {
            Value::Inline(v) => Ok(v.clone()),
            Value::Secret {
                secret_key_ref: skr,
            } => {
                let namespace = &*skr
                    .namespace
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| resource.namespace().clone().unwrap())
                    .to_string();
                let key = &skr.key;
                Api::<Secret>::namespaced(client.clone(), namespace)
                    .get(&*skr.name)
                    .await
                    .map_err(|err| {
                        Error::ActionFailed(
                            "failed to get secret".to_string(),
                            Some(Arc::new(Box::new(err))),
                        )
                    })?
                    .data
                    .unwrap_or_default()
                    .get(&skr.key)
                    .map(|d| from_utf8(&d.0).map(&str::to_string))
                    .ok_or(Error::InvalidResource(format!(
                        "secret must have data with key {key}"
                    )))?
                    .map_err(|err| {
                        Error::ActionFailed(
                            "failed to parse secret data".to_string(),
                            Some(Arc::new(Box::new(err))),
                        )
                    })
            }
        };
    }
}

///
/// A zigbee2mqtt instance.
///
#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "zigbee2mqtt.maienm.com",
    version = "v1",
    kind = "Instance",
    plural = "instances",
    namespaced,
    status = "InstanceStatus"
)]
pub struct InstanceSpec {
    /// The host of the MQTT broker.
    pub host: String,
    /// The port of the MQTT broker. Defaults to 1883.
    #[serde(default = "default_instance_port")]
    pub port: u16,
    /// The credentials to authenticate with.
    pub credentials: Option<InstanceSpecCredentials>,
    /// The base topic. Should match the mqtt.base_topic option of the Zigbee2MQTT instance. Defaults to the default used by Zigbee2MQTT ('zigbee2mqtt').
    #[serde(default = "default_instance_base_topic")]
    pub base_topic: String,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct InstanceSpecCredentials {
    /// The username to authenticate with.
    pub username: Value,
    /// The password to authenticate with.
    pub password: Value,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Eq, PartialEq, Default)]
pub struct InstanceStatus {
    /// Whether there is an active connection with the MQTT broker.
    pub broker: bool,
    /// Whether zigbee2mqtt is reachable and healthy.
    pub zigbee2mqtt: bool,
}
fn default_instance_port() -> u16 {
    1883
}
fn default_instance_base_topic() -> String {
    "zigbee2mqtt".to_string()
}

///
/// A zigbee2mqtt device.
///
#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "zigbee2mqtt.maienm.com",
    version = "v1",
    kind = "Device",
    plural = "devices",
    namespaced,
    status = "DeviceStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSpec {
    /// The instance this device belongs to. Defaults to 'default'.
    #[serde(default = "default_device_instance")]
    pub instance: String,
    /// The device address.
    pub ieee_address: String,
    /// Friendly name.
    pub friendly_name: Option<String>,
    /// Options to set.
    pub options: Option<HashMap<String, JsonValue>>,
    /// Capabilities to set.
    pub capabilities: Option<HashMap<String, JsonValue>>,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Eq, PartialEq, Default)]
pub struct DeviceStatus {
    /// Whether the device is present.
    pub exists: Option<bool>,
    /// Whether the device is in the desired state.
    pub synced: Option<bool>,
}
fn default_device_instance() -> String {
    "default".to_string()
}
