use std::sync::Arc;

use serde::Deserialize;

use super::{manager::Manager, subscription::TopicSubscription};
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
    fn to_result(self) -> Result<T, Error> {
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
        return Ok(Self {
            manager,
            subscription,
        });
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
            .map_ok(RequestResponse::to_result)
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
