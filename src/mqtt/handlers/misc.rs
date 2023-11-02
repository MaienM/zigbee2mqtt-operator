use std::{collections::HashMap, sync::Arc};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::manager::Manager;
use super::lib::{
    add_wrapper_new, BridgeRequest, BridgeRequestType, RequestResponse, TopicTracker,
    TopicTrackerType,
};
use crate::error::{Error, ErrorWithMeta};

//
// Track bridge info topic.
//
pub struct BridgeInfoTracker(Arc<TopicTracker<Self>>);
add_wrapper_new!(BridgeInfoTracker, TopicTracker);
impl BridgeInfoTracker {
    pub async fn get(&self) -> Result<BridgeInfoPayload, Error> {
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
    pub restart_required: bool,
}
#[derive(Deserialize, Debug, Clone)]
pub struct BridgeInfoConfig {
    pub devices: HashMap<String, Map<String, Value>>,
    pub groups: HashMap<usize, Map<String, Value>>,
}

///
/// Healthcheck requests.
///
pub struct HealthChecker(BridgeRequest<HealthChecker>);
add_wrapper_new!(HealthChecker, BridgeRequest);
impl HealthChecker {
    pub async fn get(&mut self) -> Result<(), ErrorWithMeta> {
        let data = self.0.request(Map::new()).await?;
        if data.healthy {
            Ok(())
        } else {
            Err(Error::ActionFailed(
                "received unhealthy response".to_string(),
                None,
            ))?
        }
    }
}
impl BridgeRequestType for HealthChecker {
    const NAME: &'static str = "health_check";
    type Request = Map<String, Value>;
    type Response = HealthcheckResponse;

    fn process_response(
        _request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>> {
        Some(response.into())
    }
}
#[derive(Deserialize)]
pub(crate) struct HealthcheckResponse {
    healthy: bool,
}

///
/// Restart Zigbee2MQTT.
///
pub struct Restarter(BridgeRequest<Restarter>);
add_wrapper_new!(Restarter, BridgeRequest);
impl Restarter {
    pub async fn run(&mut self) -> Result<(), ErrorWithMeta> {
        self.0.request(Map::new()).await?;
        Ok(())
    }
}
impl BridgeRequestType for Restarter {
    const NAME: &'static str = "restart";
    type Request = Map<String, Value>;
    type Response = Map<String, Value>;

    fn process_response(
        _request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>> {
        Some(response.into())
    }
}
