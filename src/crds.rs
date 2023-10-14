use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    pub username: String,
    /// The password to authenticate with.
    pub password: String,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Eq, PartialEq, Default)]
pub struct InstanceStatus {
    /// Whether there is an active connection with the MQTT broker.
    pub broker: bool,
}
fn default_instance_port() -> u16 {
    1883
}
fn default_instance_base_topic() -> String {
    "zigbee2mqtt".to_string()
}
