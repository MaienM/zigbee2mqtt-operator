use std::{collections::HashMap, sync::Arc};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::manager::Manager;
use super::lib::{
    add_wrapper_new, BridgeRequest, BridgeRequestType, RequestResponse, TopicTracker,
    TopicTrackerType,
};
use crate::error::Error;

//
// Track bridge info topic.
//
pub struct BridgeInfoTracker(TopicTracker<Self>);
add_wrapper_new!(BridgeInfoTracker, TopicTracker);
impl BridgeInfoTracker {
    pub async fn get(&mut self) -> Result<BridgeInfoPayload, Error> {
        self.0.get().await
    }
}
impl TopicTrackerType for BridgeInfoTracker {
    const TOPIC: &'static str = "bridge/info";
    type Payload = BridgeInfoPayload;
}
#[derive(Deserialize, Debug, Clone)]
pub struct BridgeInfoPayload {
    pub config: BridgeInfoConfig,
}
#[derive(Deserialize, Debug, Clone)]
pub struct BridgeInfoConfig {
    pub devices: HashMap<String, Map<String, Value>>,
}

///
/// Healthcheck requests.
///
pub struct HealthChecker(BridgeRequest<HealthChecker>);
add_wrapper_new!(HealthChecker, BridgeRequest);
impl HealthChecker {
    pub async fn get(&mut self) -> Result<(), Error> {
        let data = self.0.request(Map::new()).await?;
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
impl BridgeRequestType for HealthChecker {
    const NAME: &'static str = "health_check";
    type Request = Map<String, Value>;
    type Response = HealthcheckResponse;

    fn matches(_request: &Self::Request, _response: &RequestResponse<Self::Response>) -> bool {
        true
    }
}
#[derive(Deserialize)]
pub(crate) struct HealthcheckResponse {
    healthy: bool,
}