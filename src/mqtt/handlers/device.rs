use std::{fmt::Debug, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::{time::sleep, try_join};
use tracing::debug;

use super::{
    super::{
        exposes::{DeviceCapabilitiesSchema, DeviceOptionsSchema},
        manager::Manager,
        subscription::TopicSubscription,
    },
    lib::{
        add_wrapper_new, setup_configuration_manager, BridgeRequest, BridgeRequestType,
        Configuration, ConfigurationManagerInner, RequestResponse, TopicTracker, TopicTrackerType,
    },
};
use crate::{
    error::{Error, ErrorWithMeta},
    mqtt::exposes::Processor,
    with_source::ValueWithSource,
    TIMEOUT,
};

//
// Track bridge device topic.
//
pub struct BridgeDevicesTracker(Arc<TopicTracker<Self>>);
add_wrapper_new!(BridgeDevicesTracker, TopicTracker);
impl TopicTrackerType for BridgeDevicesTracker {
    const TOPIC: &'static str = "bridge/devices";
    type Payload = BridgeDevicesPayload;
}
impl BridgeDevicesTracker {
    pub async fn get_all(&self) -> Result<BridgeDevicesPayload, Error> {
        self.0.get().await
    }

    pub async fn get_device(&self, ieee_address: &str) -> Result<BridgeDevice, Error> {
        let device = self
            .get_all()
            .await?
            .into_iter()
            .find(|d| d.ieee_address == ieee_address);
        match device {
            Some(device) => {
                if !device.interview_completed {
                    Err(Error::ActionFailed(
                        "device has not yet completed Zigbee2MQTT interview".to_owned(),
                        None,
                    ))
                } else if !device.supported {
                    Err(Error::InvalidResource(
                        "device is not supported by Zigbee2MQTT".to_string(),
                    ))
                } else {
                    Ok(device)
                }
            }
            None => Err(Error::InvalidResource(
                "device is not known to Zigbee2MQTT".to_string(),
            )),
        }
    }
}
pub type BridgeDevicesPayload = Vec<BridgeDevice>;
#[derive(Deserialize, Debug, Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct BridgeDevice {
    pub ieee_address: String,
    pub friendly_name: String,
    pub interview_completed: bool,
    #[serde(rename = "type")]
    pub type_: BridgeDeviceType,
    pub supported: bool,
    pub definition: Option<BridgeDeviceDefinition>,
}
/// Type of a bridge device.
///
/// From <https://www.zigbee2mqtt.io/advanced/zigbee/01_zigbee_network.html>.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub enum BridgeDeviceType {
    /// End devices do not route traffic. They may also sleep, which makes end devices a suitable choice for battery operated devices. An end device only has one parent, either the coordinator or a router, generally the closest device when it was paired. All communications to and from the end device is via their parent. If a parent router goes offline all traffic to its children will cease until those end devices time out and attempt to find a new parent. Some models of end device, notably Xiaomi, don't attempt to find a new parent so will remain isolated until re-paired with the network.
    EndDevice,

    /// Routers are responsible for routing traffic between different nodes. Routers may not sleep. As such, routers are not a suitable choice for battery operated devices. Routers are also responsible for receiving and storing messages intended for their children. In addition to this, routers are the gate keepers to the network. They are responsible for allowing new nodes to join the network.
    Router,

    /// A coordinator is a special router. In addition to all of the router capabilities, the coordinator is responsible for forming the network. To do that, it must select the appropriate channel, PAN ID, and extended network address. It is also responsible for selecting the security mode of the network.
    ///
    /// Every network always has exactly one of these.
    Coordinator,

    /// Unknown type of device.
    #[serde(other)]
    Other,
}
#[derive(Deserialize, Debug, Clone, Default)]
pub struct BridgeDeviceDefinition {
    pub options: DeviceOptionsSchema,
    pub exposes: DeviceCapabilitiesSchema,
}

///
/// Rename devices.
///
pub struct Renamer(BridgeRequest<Renamer>);
add_wrapper_new!(Renamer, BridgeRequest);
impl Renamer {
    pub async fn run(
        &mut self,
        ieee_address: ValueWithSource<String>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        self.0
            .request(RenameRequest {
                from: ieee_address,
                to: friendly_name,
                homeassistant_rename: true,
            })
            .await?;
        Ok(())
    }
}
impl BridgeRequestType for Renamer {
    const NAME: &'static str = "device/rename";
    type Request = RenameRequest;
    type Response = RenameResponse;

    fn process_response(
        request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>> {
        match response {
            RequestResponse::Ok { ref data } => {
                if data.to == *request.to {
                    Some(response.into())
                } else {
                    None
                }
            }
            RequestResponse::Error { ref error } => {
                if error.contains(&format!("'{name}'", name = request.from)) {
                    Some(
                        response
                            .convert()
                            .map_err(|err| err.caused_by(&request.from)),
                    )
                } else if error.contains(&format!("'{name}'", name = request.to)) {
                    Some(response.convert().map_err(|err| err.caused_by(&request.to)))
                } else {
                    None
                }
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct RenameRequest {
    from: ValueWithSource<String>,
    to: ValueWithSource<String>,
    homeassistant_rename: bool,
}
#[derive(Deserialize)]
pub(crate) struct RenameResponse {
    to: String,
}

///
/// Manage device options.
///
pub struct OptionsManager {
    manager: Arc<Manager>,
    request_manager: BridgeRequest<OptionsManager>,
    ieee_address: ValueWithSource<String>,
}
impl OptionsManager {
    pub async fn new(
        manager: Arc<Manager>,
        ieee_address: ValueWithSource<String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            manager: manager.clone(),
            request_manager: BridgeRequest::new(manager).await?,
            ieee_address,
        })
    }
}
setup_configuration_manager!(OptionsManager);
#[async_trait]
impl ConfigurationManagerInner for OptionsManager {
    const NAME: &'static str = "option";

    async fn schema(&mut self) -> Result<Box<dyn Processor + Send + Sync>, ErrorWithMeta> {
        Ok(Box::new(
            self.manager
                .get_bridge_device_tracker()
                .await?
                .get_device(&self.ieee_address)
                .await
                .map_err(|err| err.caused_by(&self.ieee_address))?
                .definition
                .unwrap_or_default()
                .options,
        ))
    }

    async fn get(&mut self) -> Result<Configuration, ErrorWithMeta> {
        Ok(self
            .manager
            .get_bridge_info_tracker()
            .await?
            .get()
            .await?
            .config
            .devices
            .get(&*self.ieee_address)
            .cloned()
            .unwrap_or_default())
    }

    async fn set(
        &mut self,
        configuration: &ValueWithSource<Configuration>,
    ) -> Result<Configuration, ErrorWithMeta> {
        let value = self
            .request_manager
            .request(OptionsRequest {
                id: self.ieee_address.clone(),
                options: configuration.clone(),
            })
            .await?;
        Ok(value.to)
    }

    fn clear_property(key: &str) -> bool {
        !matches!(key, "friendly_name")
    }
}
impl BridgeRequestType for OptionsManager {
    const NAME: &'static str = "device/options";
    type Request = OptionsRequest;
    type Response = OptionsResponse;

    fn process_response(
        request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>> {
        match response {
            RequestResponse::Ok { ref data } => {
                if data.id == *request.id {
                    Some(response.into())
                } else {
                    None
                }
            }
            RequestResponse::Error { ref error } => {
                if error.contains(&format!("'{id}'", id = request.id)) {
                    Some(response.convert().map_err(|err| err.caused_by(&request.id)))
                } else {
                    None
                }
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct OptionsRequest {
    id: ValueWithSource<String>,
    options: ValueWithSource<Configuration>,
}
#[derive(Deserialize)]
pub(crate) struct OptionsResponse {
    id: String,
    to: Configuration,
}

///
/// Manage device capabilities.
///
pub struct CapabilitiesManager {
    manager: Arc<Manager>,
    ieee_address: ValueWithSource<String>,
    friendly_name: ValueWithSource<String>,
    log_subscription: TopicSubscription,
    device_subscription: TopicSubscription,
}
impl CapabilitiesManager {
    pub async fn new(
        manager: Arc<Manager>,
        ieee_address: ValueWithSource<String>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            log_subscription: manager.subscribe_topic("bridge/log", 16).await?,
            device_subscription: manager.subscribe_topic(&friendly_name, 1).await?,
            manager,
            ieee_address,
            friendly_name,
        })
    }

    async fn run<T>(&mut self, verb: &str, message: T) -> Result<CapabilitiesPayload, Error>
    where
        T: Into<Vec<u8>> + Clone + Debug,
    {
        // Drop any existing messages, we're only interested in what happens after our request.
        self.log_subscription = self.log_subscription.resubscribe();
        self.device_subscription = self.device_subscription.resubscribe();

        let topic = format!("{}/{}", self.friendly_name, verb);
        self.manager.publish(&topic, message.clone()).await?;
        let resend = self.manager.publish(&topic, message);

        // If the action succeeds this is returned on the device's topic, but if it fails an error is logged on the bridge log and nothing appears on the device's topic. Some of the errors don't include any device identification, so if the action result an an error of this type this function will result in a timeout.
        let mut device_recv = self
            .device_subscription
            .stream_swap()
            .filter_lag()
            .parse_payload::<Map<String, Value>>();
        let mut log_recv = self
            .log_subscription
            .stream_swap()
            .map_lag_to_error()
            .parse_payload_or_skip::<CapabilitiesLogResponse>()
            .filter_ok(|l| l.meta.friendly_name == *self.friendly_name);

        // We're using this try_join! in a similar way we would a select!, but with the advantage that not all branches need to cause a return (Ok => continue (until everyting is done, which will never happen here), Err => abort rest and return).
        macro_rules! terminating {
            ($body:block) => {
                async {
                    let result = async { $body }.await;
                    Result::<(), _>::Err(result)
                }
            };
        }
        macro_rules! nonterminating {
            ($body:block) => {
                async {
                    match async { $body }.await {
                        Ok(()) => Result::<(), _>::Ok(()),
                        Err(err) => Result::<(), _>::Err(Err(err)),
                    }
                }
            };
        }
        try_join! {
            nonterminating!({
                // Occasionally Zigbee2MQTT appears to miss a message (not sure which component is actually at fault for that), so re-send it once half the timeout has elapsed.
                sleep(*TIMEOUT / 2).await;
                debug!(
                    "half the timeout elapsed while attempting to {verb} current state of device {friendly_name}, resending payload",
                    friendly_name=self.friendly_name,
                );
                resend.await
            }),
            terminating!({
                sleep(*TIMEOUT).await;
                Err(Error::Zigbee2Mqtt(format!(
                    "timeout while attempting to {verb} current state of device {friendly_name}",
                    friendly_name = self.friendly_name,
                )))
            }),
            terminating!({
                // Sometimes the device capabilities that are sent right after a set are still the old values rather than the new ones, so we look at last rather than next.
                device_recv.last(*TIMEOUT, Duration::from_secs(1)).await
            }),
            terminating!({
                let log = log_recv.next_noclose().await?;
                Err(Error::Zigbee2Mqtt(format!(
                    "error while attempting to {verb} current state of device {friendly_name}: {message:?}",
                    friendly_name = self.friendly_name,
                    message = log.message,
                )))
            }),
        }.unwrap_err()
    }
}
setup_configuration_manager!(CapabilitiesManager);
#[async_trait]
impl ConfigurationManagerInner for CapabilitiesManager {
    const NAME: &'static str = "capability";

    async fn schema(&mut self) -> Result<Box<dyn Processor + Send + Sync>, ErrorWithMeta> {
        Ok(Box::new(
            self.manager
                .get_bridge_device_tracker()
                .await?
                .get_device(&self.ieee_address)
                .await
                .map_err(|err| err.caused_by(&self.ieee_address))?
                .definition
                .unwrap_or_default()
                .exposes,
        ))
    }

    async fn get(&mut self) -> Result<Configuration, ErrorWithMeta> {
        Ok(self.run("get", r#"{"state":""}"#).await?)
    }

    async fn set(
        &mut self,
        configuration: &ValueWithSource<Configuration>,
    ) -> Result<Configuration, ErrorWithMeta> {
        self.run(
            "set",
            serde_json::to_string(configuration).map_err(|err| {
                Error::ActionFailed(
                    "failed to convert capabilities to JSON".to_owned(),
                    Some(Arc::new(err)),
                )
                .caused_by(configuration)
            })?,
        )
        .await
        .map_err(|err| err.caused_by(configuration))
    }

    fn clear_property(_key: &str) -> bool {
        false
    }
}
type CapabilitiesPayload = Map<String, Value>;
#[derive(Deserialize)]
struct CapabilitiesLogResponse {
    pub message: String,
    pub meta: CapabilitiesLogResponseMeta,
}
#[derive(Deserialize)]
struct CapabilitiesLogResponseMeta {
    pub friendly_name: String,
}
