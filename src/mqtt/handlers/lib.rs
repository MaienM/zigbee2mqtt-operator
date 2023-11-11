use std::{fmt::Debug, marker::PhantomData, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::{sync::Mutex, time::timeout};

use super::super::{
    manager::Manager,
    subscription::{TopicStream, TopicSubscription},
};
use crate::{
    error::{Error, ErrorWithMeta},
    event_manager::{EventManager, EventType},
    mqtt::exposes::Processor,
    with_source::ValueWithSource,
    EventCore, TIMEOUT,
};

///
/// Track the latest version of a retained message on a topic.
///
pub(crate) struct TopicTracker<T>(Mutex<TopicTrackerInner<T>>)
where
    T: TopicTrackerType;
impl<T> TopicTracker<T>
where
    T: TopicTrackerType,
{
    pub async fn new(manager: Arc<Manager>) -> Result<Self, Error> {
        Ok(Self(Mutex::new(TopicTrackerInner::new(manager).await?)))
    }

    pub async fn get(self: &Arc<Self>) -> Result<T::Payload, Error> {
        self.0.lock().await.get().await
    }
}
pub(crate) struct TopicTrackerInner<T>
where
    T: TopicTrackerType,
{
    subscription: TopicStream<Box<dyn Stream<Item = Result<T::Payload, Error>> + Unpin + Send>>,
    value: Option<T::Payload>,
    _type: PhantomData<T>,
}
impl<T> TopicTrackerInner<T>
where
    T: TopicTrackerType,
{
    async fn new(manager: Arc<Manager>) -> Result<Self, Error> {
        let subscription = manager
            .subscribe_topic(T::TOPIC, 1)
            .await?
            .stream()
            .filter_lag()
            .parse_payload::<T::Payload>()
            .boxed();
        Ok(Self {
            subscription,
            value: None,
            _type: PhantomData,
        })
    }

    async fn get(&mut self) -> Result<T::Payload, Error> {
        if let Ok(result) =
            timeout(Duration::from_millis(1), self.subscription.next_noclose()).await
        {
            self.value = Some(result?);
        };

        match &self.value {
            Some(value) => Ok(value.clone()),
            None => {
                let value = self.subscription.next_noclose_timeout(*TIMEOUT).await?;
                self.value = Some(value.clone());
                Ok(value)
            }
        }
    }
}
pub(crate) trait TopicTrackerType {
    const TOPIC: &'static str;
    type Payload: for<'de> Deserialize<'de> + Debug + Clone + 'static;
}

//
// Zigbee2MQTT bridge requests.
//
// See <https://www.zigbee2mqtt.io/guide/usage/mqtt_topics_and_messages.html#zigbee2mqtt-bridge-request>.
//
pub(crate) struct BridgeRequest<T>
where
    T: BridgeRequestType,
{
    manager: Arc<Manager>,
    subscription: TopicSubscription,
    _type: PhantomData<T>,
}
impl<T> BridgeRequest<T>
where
    T: BridgeRequestType,
{
    pub async fn new(manager: Arc<Manager>) -> Result<Self, Error> {
        let subscription = manager
            .subscribe_topic(&format!("bridge/response/{name}", name = T::NAME), 8)
            .await?;
        Ok(Self {
            manager,
            subscription,
            _type: PhantomData,
        })
    }

    pub async fn request(&mut self, data: T::Request) -> Result<T::Response, ErrorWithMeta> {
        // Clear receive queue as anything already in it is unrelated to the request we're about to perform.
        self.subscription = self.subscription.resubscribe();

        self.manager
            .publish(
                &format!("bridge/request/{name}", name = T::NAME),
                serde_json::to_vec(&data).unwrap(),
            )
            .await?;

        self.subscription
            .stream_swap()
            .filter_lag()
            .parse_payload::<RequestResponse<T::Response>>()
            .filter_map_ok(|v| T::process_response(&data, v))
            .next_noclose_timeout(*TIMEOUT)
            .await
    }
}
pub(crate) trait BridgeRequestType {
    const NAME: &'static str;
    type Request: Serialize + Clone + Sync;
    type Response: for<'de> Deserialize<'de>;

    fn process_response(
        request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>>;
}
#[derive(Deserialize, Debug)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum RequestResponse<T> {
    Ok { data: T },
    Error { error: String },
}
impl<V> RequestResponse<V> {
    pub fn convert(self) -> Result<V, Error> {
        match self {
            RequestResponse::Ok { data } => Ok(data),
            RequestResponse::Error { error } => Err(Error::Zigbee2Mqtt(error)),
        }
    }
}
impl<V, E> From<RequestResponse<V>> for Result<V, E>
where
    E: From<Error>,
{
    fn from(value: RequestResponse<V>) -> Self {
        value.convert().map_err(E::from)
    }
}

/// Implement a `new` method for a struct that is a simple wrapper around `BridgeRequest`/`TopicTracker`.
macro_rules! add_wrapper_new {
    (
        $name:ident, $type:ident
    ) => {
        impl $name {
            pub async fn new(
                manager: Arc<crate::mqtt::Manager>,
            ) -> Result<Self, crate::error::Error> {
                return Ok(Self($type::new(manager).await?.into()));
            }
        }
    };
}
pub(crate) use add_wrapper_new;

///
/// Configuration manager.
///
#[async_trait]
pub(super) trait ConfigurationManagerInner {
    /// The (singular) name of the category of configuration being managed.
    const NAME: &'static str;

    /// Get the configuration schema.
    async fn schema(&mut self) -> Result<Box<dyn Processor + Send + Sync>, ErrorWithMeta>;

    /// Get the current configuration.
    async fn get(&mut self) -> Result<Configuration, ErrorWithMeta>;

    /// Attempt to set the configuration and return the resulting configuration.
    async fn set(
        &mut self,
        configuration: &ValueWithSource<Configuration>,
    ) -> Result<Configuration, ErrorWithMeta>;

    /// Whether to set a property which is currently set but missing from the new configuration to null.
    fn clear_property(key: &str) -> bool;
}
#[async_trait]
pub(super) trait ConfigurationManager: ConfigurationManagerInner {
    /// Update the configuration.
    async fn sync(
        &mut self,
        eventmanager: &EventManager,
        mut configuration: ValueWithSource<Configuration>,
    ) -> Result<(), ErrorWithMeta> {
        // Get the current configuration.
        let current = self.get().await?;

        // Insert null for properties in the current configuration that should be cleared.
        for key in current.keys() {
            if Self::clear_property(key) {
                configuration.entry(key.clone()).or_insert(Value::Null);
            }
        }

        // Use schema to validate/process configuration.
        let schema = self.schema().await?;
        let configuration = configuration.transform(|v| Value::from(v.take()));
        let configuration = configuration.transform(|v| schema.process(v)).transpose()?;
        let configuration: ValueWithSource<Configuration> = configuration
            .transform(|v| {
                let (value, source) = v.split();
                serde_json::from_value(value).map_err(|err| {
                    Error::ActionFailed(
                        "processing configuration yielded non-object".to_owned(),
                        Some(Arc::new(err)),
                    )
                    .caused_by(&source)
                })
            })
            .transpose()?;

        // Log what changes are going to be made.
        let differences = find_differences(&configuration, &current);
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
                        "setting {name} {key} to {wanted} (previously {actual})",
                        name = Self::NAME,
                    )),
                    reason: "Created".to_string(),
                    type_: EventType::Normal,
                    field_path: configuration
                        .source()
                        .map(|source| format!("{source}.{key}", key = difference.key)),
                })
                .await;
        }
        if differences.is_empty() {
            return Ok(());
        }

        // Apply the configuration.
        let result = self.set(&configuration).await?;

        // Verify that the result matches the desired configuration. If this is not the case, log the mismatches and return an error.
        let differences = find_differences(&configuration, &result);
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
                        name = Self::NAME,
                    )),
                    reason: "Created".to_string(),
                    type_: EventType::Warning,
                    field_path: configuration.source().map(|source| format!("{source}.{key}")),
                })
                .await;
        }
        if !differences.is_empty() {
            return Err(Error::InvalidResource(format!(
                "failed to set at least one {name}",
                name = Self::NAME
            ))
            .caused_by(&configuration)
            .mark_published());
        }

        Ok(())
    }
}
pub(crate) type Configuration = Map<String, Value>;
struct Difference {
    key: String,
    wanted: String,
    actual: String,
}
fn find_differences(wanted: &Configuration, actual: &Configuration) -> Vec<Difference> {
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

/// Implement `ConfigurationManager` for a struct.
macro_rules! setup_configuration_manager {
    ($name:ident) => {
        #[async_trait]
        impl crate::mqtt::handlers::lib::ConfigurationManager for $name {
        }
        impl $name {
            pub async fn sync(
                &mut self,
                eventmanager: &crate::event_manager::EventManager,
                configuration: crate::with_source::ValueWithSource<
                    crate::mqtt::handlers::lib::Configuration,
                >,
            ) -> Result<(), crate::error::ErrorWithMeta> {
                crate::mqtt::handlers::lib::ConfigurationManager::sync(
                    self,
                    eventmanager,
                    configuration,
                )
                .await
            }
        }
    };
}
pub(crate) use setup_configuration_manager;
