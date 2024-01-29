//! Definitions of the resources managed by this operator.

use std::{str::from_utf8, sync::Arc};

use garde::Validate;
use k8s_openapi::{api::core::v1::Secret, NamespaceResourceScope};
use kube::{Api, Client, CustomResource, Resource, ResourceExt};
use schemars::{
    gen::SchemaGenerator,
    schema::{InstanceType, Schema, SchemaObject, SingleOrVec},
    JsonSchema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    error::{Error, ErrorWithMeta},
    with_source::ValueWithSource,
    ResourceLocalExt,
};

/// An object that allows arbitrary nested fields.
///
/// This triggers the the rewrite rule introduced in <https://github.com/kube-rs/kube/pull/845>, causing the generated schema to use `x-kubernetes-preserve-unknown-fields` instead of `additionalProperties`. `additionalProperties` doesn't allow properies in nested objects, but `x-kubernetes-preserve-unknown-fields` does.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct NestedMap {
    /// The actual values.
    #[serde(flatten)]
    pub values: Map<String, Value>,

    /// This field exists only to trigger the rewrite rule.
    #[serde(rename = "___", default)]
    dummy: Option<bool>,
}

///
/// A Zigbee2MQTT instance.
///
#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema, Validate)]
#[kube(
    group = "zigbee2mqtt.maienm.com",
    version = "v1",
    kind = "Instance",
    plural = "instances",
    shortname = "zmi",
    category = "zigbee2mqtt",
    namespaced,
    status = "InstanceStatus",
    printcolumn = r#"{"name":"Broker","jsonPath":".status.broker","type":"boolean"}"#,
    printcolumn = r#"{"name":"Zigbee2MQTT","jsonPath":".status.zigbee2mqtt","type":"boolean"}"#
)]
pub struct InstanceSpec {
    /// The host of the MQTT broker.
    #[garde(length(min = 1))]
    pub host: String,

    /// The port of the MQTT broker. Defaults to 1883.
    #[serde(default = "default_instance_port")]
    #[garde(range(min = 1, max = 65535))]
    pub port: u16,

    /// The credentials to authenticate with.
    #[garde(dive)]
    pub credentials: Option<InstanceSpecCredentials>,

    /// The base topic. Should match the mqtt.base_topic option of the Zigbee2MQTT instance. Defaults to the default used by Zigbee2MQTT ('zigbee2mqtt').
    #[serde(default = "default_instance_base_topic")]
    #[garde(ascii, length(min = 1))]
    pub base_topic: String,

    /// What to do with devices that exist in Zigbee2MQTT but not in K8s.
    #[serde(default)]
    #[schemars(schema_with = "instance_handle_unmanaged_nodelete")]
    #[garde(skip)]
    pub unmanaged_devices: InstanceHandleUnmanaged,

    /// What to do with groups that exist in Zigbee2MQTT but not in K8s.
    #[serde(default)]
    #[garde(skip)]
    pub unmanaged_groups: InstanceHandleUnmanaged,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Validate)]
#[allow(missing_docs)]
pub struct InstanceSpecCredentials {
    /// The username to authenticate with.
    #[garde(dive)]
    pub username: ValueFrom,

    /// The password to authenticate with.
    #[garde(dive)]
    pub password: ValueFrom,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Eq, PartialEq, Default)]
#[allow(missing_docs)]
pub struct InstanceStatus {
    /// Whether there is an active connection with the MQTT broker.
    pub broker: bool,

    /// Whether zigbee2mqtt is reachable and healthy.
    pub zigbee2mqtt: Option<bool>,
}
/// How to handle a resource that exists in Zigbee2MQTT but not in K8s.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceHandleUnmanaged {
    /// Ignore unmanaged resource.
    Ignore,

    /// Log unmanaged resource.
    #[default]
    Log,

    /// Delete unmanaged resource.
    Delete,
}
fn instance_handle_unmanaged_nodelete(_: &mut SchemaGenerator) -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
        enum_values: Some(vec![
            serde_json::to_value(InstanceHandleUnmanaged::Ignore).unwrap(),
            serde_json::to_value(InstanceHandleUnmanaged::Log).unwrap(),
        ]),
        ..SchemaObject::default()
    })
}
fn default_instance_port() -> u16 {
    1883
}
fn default_instance_base_topic() -> String {
    "zigbee2mqtt".to_string()
}
fn default_instance() -> String {
    "default".to_string()
}
/// Trait for CRDs that belong to a specific [`Instance`].
pub trait Instanced {
    /// Get the name of the instance this resource belongs to, in the format of [`ResourceLocalExt::full_name`].
    fn get_instance_fullname(&self) -> ValueWithSource<String>;
}

///
/// A Zigbee2MQTT device.
///
/// See <https://www.zigbee2mqtt.io/guide/configuration/devices-groups.html#devices-and-groups>.
///
#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema, Validate)]
#[kube(
    group = "zigbee2mqtt.maienm.com",
    version = "v1",
    kind = "Device",
    plural = "devices",
    shortname = "zmd",
    category = "zigbee2mqtt",
    namespaced,
    status = "DeviceStatus",
    printcolumn = r#"{"name":"Exists","jsonPath":".status.exists","type":"boolean"}"#,
    printcolumn = r#"{"name":"Synced","jsonPath":".status.synced","type":"boolean"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSpec {
    /// The instance this device belongs to. Defaults to 'default'.
    #[serde(default = "default_instance")]
    #[garde(ascii, length(min = 1, max = 63))]
    pub instance: String,

    /// The device address.
    #[garde(ascii, length(min = 3))]
    pub ieee_address: String,

    /// Friendly name.
    #[garde(ascii, length(min = 1))]
    pub friendly_name: Option<String>,

    /// The authorative options for the device.
    ///
    /// This is a combination of the common settings and the device specific settings, without `friendly_name`.
    ///
    /// The common settings can be found in the 'Settings' tab in the UI, with more details in the [common device options](https://www.zigbee2mqtt.io/guide/configuration/devices-groups.html#common-device-options) section in the documentation.
    ///
    /// The device specific settings can be found in the 'Settings (specific)' tab in the UI, with more details in the [supported devices listings](https://www.zigbee2mqtt.io/supported-devices/).
    ///
    /// Note that unset/null and `{}` are different; the former will not manage options for this device at all while the latter will set all options to their default values.
    #[garde(skip)]
    pub options: Option<NestedMap>,

    /// Capabilities to set.
    ///
    /// The available capabilities can be found in the 'Exposes' tab in the settings, with more details in the [supported devices listings](https://www.zigbee2mqtt.io/supported-devices/).
    #[garde(skip)]
    pub capabilities: Option<NestedMap>,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Eq, PartialEq, Default)]
#[allow(missing_docs)]
pub struct DeviceStatus {
    /// Whether the device is present.
    pub exists: Option<bool>,

    /// Whether the device is in the desired state.
    pub synced: Option<bool>,
}
impl Instanced for Device {
    fn get_instance_fullname(&self) -> ValueWithSource<String> {
        ValueWithSource::new(
            format!("{}/{}", self.namespace().unwrap(), self.spec.instance),
            Some("spec.instance".to_owned()),
        )
    }
}

///
/// A Zigbee2MQTT group.
///
/// See <https://www.zigbee2mqtt.io/guide/usage/groups.html>.
///
#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema, Validate)]
#[kube(
    group = "zigbee2mqtt.maienm.com",
    version = "v1",
    kind = "Group",
    plural = "groups",
    shortname = "zmg",
    category = "zigbee2mqtt",
    namespaced,
    status = "GroupStatus",
    printcolumn = r#"{"name":"Exists","jsonPath":".status.exists","type":"boolean"}"#,
    printcolumn = r#"{"name":"Synced","jsonPath":".status.synced","type":"boolean"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct GroupSpec {
    /// The instance this group belongs to. Defaults to 'default'.
    #[serde(default = "default_instance")]
    #[garde(ascii, length(min = 1, max = 63))]
    pub instance: String,

    /// The name of the group.
    #[garde(ascii, length(min = 1))]
    pub friendly_name: String,

    /// The ID of the group.
    ///
    /// If not provided a random id will be generated for the group.
    #[garde(range(min = 1))]
    pub id: Option<usize>,

    /// `ieee_address`es or `friendly_name`s of [`Device`]s that should be a member of this group.
    #[garde(inner(inner(ascii, length(min = 1))))]
    pub members: Option<Vec<String>>,

    /// The authorative options for the group.
    ///
    /// The available options can be found in [the groups section of the documentation](https://www.zigbee2mqtt.io/guide/usage/groups.html#configuration).
    ///
    /// Note that unset/null and `{}` are different; the former will not manage options for this group at all while the latter will set all options to their default values.
    #[garde(skip)]
    pub options: Option<NestedMap>,
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Eq, PartialEq, Default)]
#[allow(missing_docs)]
pub struct GroupStatus {
    /// Whether the group exist.
    pub exists: Option<bool>,

    /// Whether the group is in the desired state.
    pub synced: Option<bool>,

    /// The group's ID.
    pub id: Option<usize>,
}
impl Instanced for Group {
    fn get_instance_fullname(&self) -> ValueWithSource<String> {
        ValueWithSource::new(
            format!("{}/{}", self.namespace().unwrap(), self.spec.instance),
            Some("spec.instance".to_owned()),
        )
    }
}

///
/// A value that can be indirect, like environment variables.
///
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Validate)]
#[allow(missing_docs)]
pub enum ValueFrom {
    /// An inline value.
    #[serde(rename = "value")]
    Inline(#[garde(length(min = 1))] String),

    /// A value stored in a secret.
    #[serde(rename = "valueFrom", rename_all = "camelCase")]
    Secret {
        #[garde(dive)]
        secret_key_ref: SecretKeyRef,
    },
}
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Validate)]
#[allow(missing_docs)]
pub struct SecretKeyRef {
    /// The namespace of the secret.
    ///
    /// If left empty the secret is assumed to be in the same namespace as the resource referring to it.
    #[garde(ascii, length(min = 1, max = 63))]
    namespace: Option<String>,

    /// The name of the secret.
    #[garde(ascii, length(min = 1, max = 63))]
    name: String,

    /// The key of the value in the secret's data to use.
    #[garde(ascii)]
    key: String,
}
impl ValueWithSource<ValueFrom> {
    /// Get the current value.
    ///
    /// # Errors
    ///
    /// Will return [`Error`] if the value contains a reference that cannot be resolved.
    pub async fn get<T>(
        &self,
        client: &Client,
        resource: &T,
    ) -> Result<ValueWithSource<String>, ErrorWithMeta>
    where
        T: Resource<Scope = NamespaceResourceScope> + ResourceLocalExt,
    {
        match &**self {
            ValueFrom::Inline(v) => Ok(self.with_value(v.clone())),
            ValueFrom::Secret {
                secret_key_ref: skr,
            } => {
                let namespace = skr
                    .namespace
                    .as_ref()
                    .cloned()
                    .or_else(|| resource.namespace().clone())
                    .ok_or_else(||
                        Error::InvalidResource("resource does not have a namespace, so reference must specify namespace".to_owned()).caused_by(&self.sub((), ".secretKeyRef.namespace"))
                    )?;
                let secret = Api::<Secret>::namespaced(client.clone(), &namespace)
                    .get(&skr.name)
                    .await
                    .map_err(|err| {
                        Error::ActionFailed("failed to get secret".to_string(), Some(Arc::new(err)))
                            .caused_by(&self.sub((), ".secretKeyRef"))
                    })?;
                let value = secret
                    .data
                    .unwrap_or_default()
                    .get(&skr.key)
                    .map(|d| from_utf8(&d.0).map(str::to_string))
                    .ok_or(
                        Error::InvalidResource("secret does not have data for this key".to_owned())
                            .caused_by(&self.sub((), ".secretKeyRef.key")),
                    )?
                    .map_err(|err| {
                        Error::ActionFailed(
                            "failed to parse secret data for this key".to_string(),
                            Some(Arc::new(err)),
                        )
                        .caused_by(&self.sub((), ".secretKeyRef.key"))
                    })?;
                Ok(self.with_value(value))
            }
        }
    }
}
