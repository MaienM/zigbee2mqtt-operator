use std::{any::type_name, mem, sync::Arc, time::Duration};

use derive_more::{Deref, DerefMut};
use futures::Stream;
use rumqttc::Publish;
use serde::Deserialize;
use tokio::{sync::broadcast, time::timeout};
use tokio_stream::{
    wrappers::{errors::BroadcastStreamRecvError, BroadcastStream},
    StreamExt,
};

use crate::Error;

/// A subscription to an MQTT topic.
#[derive(Deref, DerefMut)]
pub struct TopicSubscription {
    #[deref]
    #[deref_mut]
    receiver: broadcast::Receiver<Publish>,
    topic: String,
}
impl TopicSubscription {
    pub fn new(receiver: broadcast::Receiver<Publish>, topic: String) -> TopicSubscription {
        Self { receiver, topic }
    }

    /// Create a copy of this subscription. This copy will not get any of the currently pending messages, it will only get messages that are received after its creation.
    pub fn resubscribe(&self) -> Self {
        Self {
            receiver: self.receiver.resubscribe(),
            topic: self.topic.clone(),
        }
    }

    /// Create a TopicStream from this subscription, consuming it in the process.
    pub fn stream(self) -> TopicStream<BroadcastStream<Publish>> {
        TopicStream {
            stream: BroadcastStream::new(self.receiver),
            topic: self.topic,
        }
    }

    /// Create a TopicStream from this subscription, leaving a new copy in its place (as would be returned from resubscribe).
    pub fn stream_swap(&mut self) -> TopicStream<BroadcastStream<Publish>> {
        let mut receiver = self.receiver.resubscribe();
        mem::swap(&mut receiver, &mut self.receiver);
        TopicStream {
            stream: BroadcastStream::new(receiver),
            topic: self.topic.clone(),
        }
    }
}

/// A streaming wrapper for a subscription to an MQTT topic.
pub struct TopicStream<St> {
    stream: St,
    topic: String,
}
impl<V, St> TopicStream<St>
where
    St: Stream<Item = Result<V, BroadcastStreamRecvError>> + StreamExt,
{
    /// Filter out any lag events.
    pub fn filter_lag(self) -> TopicStream<impl Stream<Item = Result<V, Error>>> {
        TopicStream {
            stream: self.stream.filter_map(|value| match value {
                Ok(p) => Some(Ok(p)),
                Err(BroadcastStreamRecvError::Lagged(_)) => None,
            }),
            topic: self.topic,
        }
    }

    /// Convert lag events to errors.
    pub fn map_lag_to_error(self) -> TopicStream<impl Stream<Item = Result<V, Error>>> {
        let topic = self.topic.clone();
        TopicStream {
            stream: self.stream.map(move |value| match value {
                Ok(p) => Ok(p),
                Err(BroadcastStreamRecvError::Lagged(_)) => Err(Error::SubscriptionError {
                    topic: topic.clone(),
                    message: "subscription lagged, some messages have been lost".to_string(),
                    source: None,
                }),
            }),
            topic: self.topic,
        }
    }
}
impl<St> TopicStream<St>
where
    St: Stream<Item = Result<Publish, Error>> + StreamExt,
{
    /// Parse the payload or convert to an error if parsing fails.
    pub fn parse_payload<T>(self) -> TopicStream<impl Stream<Item = Result<T, Error>>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let topic = self.topic.clone();
        self.map_ok(move |value| {
            serde_json::from_slice::<T>(&value.payload).map_err(|err| Error::SubscriptionError {
                topic: topic.clone(),
                message: format!(
                    "failed to parse message {payload:?} to {type_}",
                    payload = value.payload,
                    type_ = type_name::<T>(),
                ),
                source: Some(Arc::new(Box::new(err))),
            })
        })
    }

    /// Parse the payload or filter out the message if the parsing fails.
    pub fn parse_payload_or_skip<T>(self) -> TopicStream<impl Stream<Item = Result<T, Error>>>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.filter_map_ok(|value| match serde_json::from_slice::<T>(&value.payload) {
            Ok(value) => Some(Ok(value)),
            Err(_) => None,
        })
    }
}
impl<V, St> TopicStream<St>
where
    St: Stream<Item = Result<V, Error>> + StreamExt,
{
    /// Filter non-errors.
    pub fn filter_ok<F>(self, mut f: F) -> TopicStream<impl Stream<Item = Result<V, Error>>>
    where
        F: FnMut(&V) -> bool,
    {
        TopicStream {
            stream: self.stream.filter(move |value| match value {
                Ok(value) => f(&value),
                Err(_) => true,
            }),
            topic: self.topic,
        }
    }

    /// Map non-errors.
    pub fn map_ok<NV, F>(self, mut f: F) -> TopicStream<impl Stream<Item = Result<NV, Error>>>
    where
        F: FnMut(V) -> Result<NV, Error>,
    {
        TopicStream {
            stream: self.stream.map(move |value| match value {
                Ok(value) => f(value),
                Err(err) => Err(err),
            }),
            topic: self.topic,
        }
    }

    /// Filter & map non-errors.
    pub fn filter_map_ok<NV, F>(
        self,
        mut f: F,
    ) -> TopicStream<impl Stream<Item = Result<NV, Error>>>
    where
        F: FnMut(V) -> Option<Result<NV, Error>>,
    {
        TopicStream {
            stream: self.stream.filter_map(move |value| match value {
                Ok(value) => f(value),
                Err(err) => Some(Err(err)),
            }),
            topic: self.topic,
        }
    }

    /// Wrap the stream in a Box.
    pub fn boxed(self) -> TopicStream<Box<dyn Stream<Item = Result<V, Error>> + Unpin + Send>>
    where
        St: Unpin + Send + 'static,
    {
        TopicStream {
            stream: Box::new(self.stream),
            topic: self.topic,
        }
    }
}
impl<V, St> TopicStream<St>
where
    St: Stream<Item = Result<V, Error>> + StreamExt + Unpin + Send,
{
    /// Get the next value, or None if the stream has closed.
    pub async fn next(&mut self) -> Option<St::Item> {
        self.stream.next().await
    }

    /// As next(), but returns an error if the stream has ended.
    pub async fn next_noclose(&mut self) -> St::Item {
        match self.next().await {
            Some(result) => result,
            None => Err(Error::SubscriptionError {
                topic: self.topic.clone(),
                message: "subscription closed".to_string(),
                source: None,
            }),
        }
    }

    /// As next_noclose(), but returns an error if no item is read within the given timeout.
    pub async fn next_noclose_timeout(&mut self, duration: Duration) -> St::Item {
        match timeout(duration, self.next_noclose()).await {
            Ok(value) => value,
            Err(_) => Err(Error::SubscriptionError {
                topic: self.topic.clone(),
                message: "timeout while waiting for message".to_string(),
                source: None,
            }),
        }
    }

    /// Keep reading the next item from the stream until:
    /// - the result is an error (which will be returned immediately).
    /// - getting the next item takes longer than the provided timeout (at which point the last item will be returned, or an error if no items were received within the timeout).
    /// - the steam closes (which will result in an error, as next_noclose).
    pub async fn last(&mut self, timeout_first: Duration, timeout_interval: Duration) -> St::Item {
        let mut result = Ok(self.next_noclose_timeout(timeout_first).await?);
        loop {
            match timeout(timeout_interval, self.next_noclose()).await {
                Ok(item) => {
                    result = Ok(item?);
                }
                Err(_) => break,
            }
        }
        result
    }
}