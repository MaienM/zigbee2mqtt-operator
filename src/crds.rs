use std::{collections::HashMap, str::from_utf8, sync::Arc};

use k8s_openapi::{api::core::v1::Secret, NamespaceResourceScope};
use kube::{Api, Client, CustomResource, Resource, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{ext::ResourceLocalExt, Error};

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
    /// Options to set. This is a combination of the common settings and the device specific settings, except for `friendly_name`.
    ///
    /// The common settings can be found in the 'Settings' tab in the UI, with more details in the [common device options](https://www.zigbee2mqtt.io/guide/configuration/devices-groups.html#common-device-options) section in the documentation.
    ///
    /// The device specific settings can be found in the 'Settings (specific)' tab in the UI, with more details in the [supported devices listings](https://www.zigbee2mqtt.io/supported-devices/).
    pub options: Option<HashMap<String, JsonValue>>,
    /// Capabilities to set.
    ///
    /// The available capabilities can be found in the 'Exposes' tab in the settings, with more details in the [supported devices listings](https://www.zigbee2mqtt.io/supported-devices/).
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
    ///
    /// # Errors
    ///
    /// Will return [`Error`] if the value contains a reference that cannot be resolved.
    pub async fn get<T>(&self, client: &Client, resource: &T) -> Result<String, Error>
    where
        T: Resource<Scope = NamespaceResourceScope> + ResourceLocalExt,
    {
        match self {
            Value::Inline(v) => Ok(v.clone()),
            Value::Secret {
                secret_key_ref: skr,
            } => {
                let namespace = &*skr
                    .namespace
                    .as_ref()
                    .cloned()
                    .or_else(|| resource.namespace().clone())
                    .ok_or_else(|| Error::InvalidResource(format!("parent resource {id} does not have a namespace, so reference must specify namespace", id=resource.id())))?
                    .to_string();
                let key = &skr.key;
                let secret = Api::<Secret>::namespaced(client.clone(), namespace)
                    .get(&skr.name)
                    .await
                    .map_err(|err| {
                        Error::ActionFailed(
                            "failed to get secret".to_string(),
                            Some(Arc::new(Box::new(err))),
                        )
                    })?;
                secret
                    .data
                    .unwrap_or_default()
                    .get(&skr.key)
                    .map(|d| from_utf8(&d.0).map(str::to_string))
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
        }
    }
}
