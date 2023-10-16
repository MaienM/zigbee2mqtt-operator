use std::{collections::HashMap, fmt::Debug, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    sync::Mutex,
    time::{sleep, timeout},
    try_join,
};
use tracing::debug;

use super::{
    manager::Manager,
    subscription::{TopicStream, TopicSubscription},
};
use crate::{Error, TIMEOUT};

pub trait Handler: Sized {
    type Result;
}

// For responses to bridge requests.
#[derive(Deserialize, Debug)]
#[serde(tag = "status", rename_all = "lowercase")]
enum RequestResponse<T> {
    Ok { data: T },
    Error { error: String },
}
impl<T> RequestResponse<T> {
    fn convert(self) -> Result<T, Error> {
        match self {
            RequestResponse::Ok { data } => Ok(data),
            RequestResponse::Error { error } => Err(Error::Zigbee2MQTTError(error)),
        }
    }
}

///
/// Zigbee2MQTT health check requests.
///
pub struct HealthChecker {
    manager: Arc<Manager>,
    subscription: TopicSubscription,
}
impl Handler for HealthChecker {
    type Result = ();
}
impl HealthChecker {
    pub async fn new(manager: Arc<Manager>) -> Result<Self, Error> {
        let subscription = manager
            .subscribe_topic("bridge/response/health_check", 1)
            .await?;
        Ok(Self {
            manager,
            subscription,
        })
    }

    pub async fn get(&mut self) -> Result<<HealthChecker as Handler>::Result, Error> {
        // Clear receive queue in case something else is also making healthcheck requests.
        self.subscription = self.subscription.resubscribe();

        self.manager
            .publish("bridge/request/health_check", "{}")
            .await?;

        let data = self
            .subscription
            .stream_swap()
            .filter_lag()
            .parse_payload::<RequestResponse<HealthcheckPayload>>()
            .map_ok(RequestResponse::convert)
            .next_noclose_timeout(TIMEOUT)
            .await?;

        if data.healthy {
            Ok(())
        } else {
            Err(Error::ActionFailed(
                "received unhealthy response".to_string(),
                None,
            ))
        }
    }
}
#[derive(Deserialize)]
struct HealthcheckPayload {
    healthy: bool,
}

///
/// Track the latest version of a retained message on a topic.
///
pub struct TopicTracker<T> {
    subscription: TopicStream<Box<dyn Stream<Item = Result<T, Error>> + Unpin + Send>>,
    value: Option<T>,
}
impl<T> TopicTracker<T>
where
    T: for<'de> Deserialize<'de> + Clone + 'static,
{
    pub async fn new(manager: Arc<Manager>, topic: &str) -> Result<Self, Error> {
        let subscription = manager
            .subscribe_topic(topic, 1)
            .await?
            .stream()
            .filter_lag()
            .parse_payload::<T>()
            .boxed();
        Ok(Self {
            subscription,
            value: None,
        })
    }

    pub async fn get(&mut self) -> Result<T, Error>
    where
        T: Debug,
    {
        if let Ok(result) =
            timeout(Duration::from_millis(1), self.subscription.next_noclose()).await
        {
            self.value = Some(result?);
        };

        match &self.value {
            Some(value) => Ok(value.clone()),
            None => {
                let value = self.subscription.next_noclose_timeout(TIMEOUT).await?;
                self.value = Some(value.clone());
                Ok(value)
            }
        }
    }
}
macro_rules! create_topic_tracker {
    (
        $(#[$meta:meta])*
        $name:ident, $type:ident, $topic:expr
    ) => {
        $(#[$meta])*
        pub struct $name(TopicTracker<$type>);
        #[async_trait]
        impl Handler for $name {
            type Result = $type;
        }
        impl $name {
            pub async fn new(manager: Arc<Manager>) -> Result<Self, Error> {
                return Ok(Self(TopicTracker::new(manager, $topic).await?));
            }

            pub async fn get(&mut self) -> Result<<Self as Handler>::Result, Error> {
                self.0.get().await
            }
        }
    };
}

//
// Track bridge device topic.
//
create_topic_tracker!(
    /// Track bridge device topic.
    BridgeDevicesTracker,
    BridgeDevicesPayload,
    "bridge/devices"
);
impl BridgeDevicesTracker {
    pub async fn get_device(&mut self, ieee_address: &str) -> Result<BridgeDevice, Error> {
        let device = self
            .get()
            .await?
            .into_iter()
            .find(|d| d.ieee_address == ieee_address);
        match device {
            Some(device) => {
                if !device.interview_completed {
                    Err(Error::ActionFailed(
                        format!("device with ieee_address {ieee_address} is still being added to Zigbee2MQTT"),
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
pub struct BridgeDevice {
    pub ieee_address: String,
    pub friendly_name: String,
    pub interview_completed: bool,
    pub supported: bool,
}

//
// Track bridge info topic.
//
create_topic_tracker!(
    /// Track bridge info topic.
    BridgeInfoTracker,
    BridgeInfoPayload,
    "bridge/info"
);
impl BridgeInfoTracker {
    pub async fn get_device_options(
        &mut self,
        ieee_address: &str,
    ) -> Result<HashMap<String, Value>, Error> {
        Ok(self
            .get()
            .await?
            .config
            .devices
            .get(ieee_address)
            .cloned()
            .unwrap_or_default())
    }
}
#[derive(Deserialize, Debug, Clone)]
pub struct BridgeInfoPayload {
    pub config: BridgeInfoConfig,
}
#[derive(Deserialize, Debug, Clone)]
pub struct BridgeInfoConfig {
    pub devices: HashMap<String, HashMap<String, Value>>,
}
///
/// Zigbee2MQTT device rename request.
///
pub struct DeviceRenamer {
    manager: Arc<Manager>,
}
impl Handler for DeviceRenamer {
    type Result = ();
}
impl DeviceRenamer {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }

    pub async fn run(
        &mut self,
        ieee_address: &str,
        friendly_name: &str,
    ) -> Result<<DeviceRenamer as Handler>::Result, Error> {
        let subscription = self
            .manager
            .subscribe_topic("bridge/response/device/rename", 8)
            .await?;

        self.manager
            .publish(
                "bridge/request/device/rename",
                serde_json::to_vec(&json!({
                    "from": ieee_address,
                    "to": friendly_name,
                    "homeassistant_rename": true,
                }))
                .unwrap(),
            )
            .await?;

        subscription
            .stream()
            .filter_lag()
            .parse_payload::<RequestResponse<DeviceRenamePayload>>()
            .filter_ok(|p| match p {
                RequestResponse::Ok { data } => data.to == friendly_name,
                RequestResponse::Error { error } => {
                    error == &format!("friendly_name '{friendly_name}' is already in use")
                }
            })
            .map_ok(RequestResponse::convert)
            .next_noclose_timeout(TIMEOUT)
            .await?;

        Ok(())
    }
}
#[derive(Deserialize)]
struct DeviceRenamePayload {
    to: String,
}

///
/// Manage device options.
///
pub struct DeviceOptionsManager {
    manager: Arc<Manager>,
    tracker: Arc<Mutex<BridgeInfoTracker>>,
    ieee_address: String,
}
impl Handler for DeviceOptionsManager {
    type Result = HashMap<String, Value>;
}
impl DeviceOptionsManager {
    pub fn new(
        manager: Arc<Manager>,
        tracker: Arc<Mutex<BridgeInfoTracker>>,
        ieee_address: String,
    ) -> Self {
        Self {
            manager,
            tracker,
            ieee_address,
        }
    }

    pub async fn get(&mut self) -> Result<<Self as Handler>::Result, Error> {
        Ok(self
            .tracker
            .lock()
            .await
            .get()
            .await?
            .config
            .devices
            .get(&self.ieee_address)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn set(
        &mut self,
        options: &<Self as Handler>::Result,
    ) -> Result<<Self as Handler>::Result, Error> {
        let subscription = self
            .manager
            .subscribe_topic("bridge/response/device/options", 8)
            .await?;

        self.manager
            .publish(
                "bridge/request/device/options",
                serde_json::to_vec(&json!({
                    "id": self.ieee_address,
                    "options": options,
                }))
                .unwrap(),
            )
            .await?;

        let value = subscription
            .stream()
            .filter_lag()
            .parse_payload::<RequestResponse<DeviceOptionsPayload>>()
            .filter_ok(|p| match p {
                RequestResponse::Ok { data } => data.id == self.ieee_address,
                RequestResponse::Error { error } => error.contains(&format!(
                    "'{ieee_address}'",
                    ieee_address = self.ieee_address
                )),
            })
            .map_ok(RequestResponse::convert)
            .next_noclose_timeout(TIMEOUT)
            .await?;

        Ok(value.to)
    }
}
#[derive(Deserialize)]
struct DeviceOptionsPayload {
    id: String,
    to: <DeviceOptionsManager as Handler>::Result,
}

///
/// Manage device capabilities.
///
pub struct DeviceCapabilitiesManager {
    manager: Arc<Manager>,
    friendly_name: String,
    log_subscription: TopicSubscription,
    device_subscription: TopicSubscription,
}
impl Handler for DeviceCapabilitiesManager {
    type Result = DeviceCapabilitiesPayload;
}
impl DeviceCapabilitiesManager {
    pub async fn new(manager: Arc<Manager>, friendly_name: String) -> Result<Self, Error> {
        Ok(Self {
            log_subscription: manager.subscribe_topic("bridge/log", 16).await?,
            device_subscription: manager.subscribe_topic(&friendly_name, 1).await?,
            manager,
            friendly_name,
        })
    }

    async fn run<T>(&mut self, verb: &str, message: T) -> Result<<Self as Handler>::Result, Error>
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
            .parse_payload::<HashMap<String, Value>>();
        let mut log_recv = self
            .log_subscription
            .stream_swap()
            .map_lag_to_error()
            .parse_payload_or_skip::<DeviceCapabilitiesLogResponse>()
            .filter_ok(|l| l.meta.friendly_name == self.friendly_name);

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
                        Ok(_) => Result::<(), _>::Ok(()),
                        Err(err) => Result::<(), _>::Err(Err(err)),
                    }
                }
            };
        }
        try_join! {
            nonterminating!({
                // Occasionally Zigbee2MQTT appears to miss a message (not sure which component is actually at fault for that), so re-send it once half the timeout has elapsed.
                sleep(TIMEOUT / 2).await;
                debug!(
                    "half the timeout elapsed while attempting to {verb} current state of device {friendly_name}, resending payload",
                    friendly_name=self.friendly_name,
                );
                resend.await
            }),
            terminating!({
                sleep(TIMEOUT).await;
                Err(Error::Zigbee2MQTTError(format!(
                    "timeout while attempting to {verb} current state of device {friendly_name}",
                    friendly_name = self.friendly_name,
                )))
            }),
            terminating!({
                // Sometimes the device capabilities that are sent right after a set are still the old values rather than the new ones, so we look at last rather than next.
                device_recv.last(TIMEOUT, Duration::from_secs(1)).await
            }),
            terminating!({
                let log = log_recv.next_noclose().await?;
                Err(Error::Zigbee2MQTTError(format!(
                    "error while attempting to {verb} current state of device {friendly_name}: {message:?}",
                    friendly_name = self.friendly_name,
                    message = log.message,
                )))
            }),
        }.unwrap_err()
    }

    pub async fn get(&mut self) -> Result<<Self as Handler>::Result, Error> {
        self.run("get", r#"{"state":""}"#).await
    }

    pub async fn set<T>(&mut self, message: T) -> Result<<Self as Handler>::Result, Error>
    where
        T: Into<Vec<u8>> + Debug + Clone,
    {
        self.run("set", message).await
    }
}
type DeviceCapabilitiesPayload = HashMap<String, Value>;
#[derive(Deserialize)]
struct DeviceCapabilitiesLogResponse {
    pub message: String,
    pub meta: DeviceCapabilitiesLogResponseMeta,
}
#[derive(Deserialize)]
struct DeviceCapabilitiesLogResponseMeta {
    pub friendly_name: String,
}
