use std::{fmt::Debug, marker::PhantomData, sync::Arc, time::Duration};

use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use super::super::{
    manager::Manager,
    subscription::{TopicStream, TopicSubscription},
};
use crate::{error::Error, TIMEOUT};

///
/// Track the latest version of a retained message on a topic.
///
pub(crate) struct TopicTracker<T>
where
    T: TopicTrackerType,
{
    subscription: TopicStream<Box<dyn Stream<Item = Result<T::Payload, Error>> + Unpin + Send>>,
    value: Option<T::Payload>,
    _type: PhantomData<T>,
}
impl<T> TopicTracker<T>
where
    T: TopicTrackerType,
{
    pub async fn new(manager: Arc<Manager>) -> Result<Self, Error> {
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

    pub async fn get(&mut self) -> Result<T::Payload, Error> {
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

    pub async fn request(&mut self, data: T::Request) -> Result<T::Response, Error> {
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
            .filter_ok(|v| T::matches(&data, v))
            .map_ok(RequestResponse::convert)
            .next_noclose_timeout(TIMEOUT)
            .await
    }
}
pub(crate) trait BridgeRequestType {
    const NAME: &'static str;
    type Request: Serialize + Clone + Sync;
    type Response: for<'de> Deserialize<'de>;

    fn matches(request: &Self::Request, response: &RequestResponse<Self::Response>) -> bool;
}
#[derive(Deserialize, Debug)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum RequestResponse<T> {
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

/// Implement a `new` method for a struct that is a simple wrapper around `BridgeRequest`/`TopicTracker`.
macro_rules! add_wrapper_new {
    (
        $name:ident, $type:ident
    ) => {
        impl $name {
            pub async fn new(manager: Arc<Manager>) -> Result<Self, Error> {
                return Ok(Self($type::new(manager).await?));
            }
        }
    };
}
pub(crate) use add_wrapper_new;
