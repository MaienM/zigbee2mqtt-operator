use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{
    super::manager::Manager,
    lib::{
        add_wrapper_new, BridgeRequest, BridgeRequestType, RequestResponse, TopicTracker,
        TopicTrackerType,
    },
};
use crate::error::Error;

//
// Track bridge group topic.
//
pub struct BridgeGroupsTracker(Arc<TopicTracker<Self>>);
add_wrapper_new!(BridgeGroupsTracker, TopicTracker);
impl TopicTrackerType for BridgeGroupsTracker {
    const TOPIC: &'static str = "bridge/groups";
    type Payload = BridgeGroupsPayload;
}
impl BridgeGroupsTracker {
    pub async fn get_by_id(&self, id: usize) -> Result<Option<BridgeGroup>, Error> {
        Ok(self.0.get().await?.into_iter().find(|g| g.id == id))
    }

    pub async fn get_by_friendly_name(
        &self,
        friendly_name: &str,
    ) -> Result<Option<BridgeGroup>, Error> {
        Ok(self
            .0
            .get()
            .await?
            .into_iter()
            .find(|g| g.friendly_name == friendly_name))
    }
}
pub type BridgeGroupsPayload = Vec<BridgeGroup>;
#[derive(Deserialize, Debug, Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct BridgeGroup {
    pub id: usize,
    pub friendly_name: String,
    pub members: Vec<BridgeGroupMember>,
}
#[derive(Deserialize, Debug, Clone)]
pub struct BridgeGroupMember {
    pub endpoint: usize,
    pub ieee_address: String,
}

///
/// Create groups.
///
pub struct Creator(BridgeRequest<Creator>);
add_wrapper_new!(Creator, BridgeRequest);
impl Creator {
    pub async fn run(&mut self, id: Option<usize>, friendly_name: &str) -> Result<usize, Error> {
        let value = self
            .0
            .request(CreateRequest {
                id,
                friendly_name: friendly_name.to_owned(),
            })
            .await?;
        Ok(value.id)
    }
}
impl BridgeRequestType for Creator {
    const NAME: &'static str = "group/add";
    type Request = CreateRequest;
    type Response = CreateResponse;

    fn matches(request: &Self::Request, response: &RequestResponse<Self::Response>) -> bool {
        match response {
            RequestResponse::Ok { data } => data.friendly_name == request.friendly_name,
            RequestResponse::Error { error } => {
                error.contains(&format!("'{name}'", name = request.friendly_name))
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct CreateRequest {
    id: Option<usize>,
    friendly_name: String,
}
#[derive(Deserialize)]
pub(crate) struct CreateResponse {
    id: usize,
    friendly_name: String,
}

///
/// Delete groups.
///
pub struct Deletor(BridgeRequest<Deletor>);
add_wrapper_new!(Deletor, BridgeRequest);
impl Deletor {
    pub async fn run(&mut self, id: usize) -> Result<(), Error> {
        self.0.request(DeleteRequestResponse { id }).await?;
        Ok(())
    }
}
impl BridgeRequestType for Deletor {
    const NAME: &'static str = "group/remove";
    type Request = DeleteRequestResponse;
    type Response = DeleteRequestResponse;

    fn matches(request: &Self::Request, response: &RequestResponse<Self::Response>) -> bool {
        match response {
            RequestResponse::Ok { data } => data.id == request.id,
            RequestResponse::Error { error } => {
                error.contains(&format!(r#""groupid":{id}"#, id = request.id))
            }
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct DeleteRequestResponse {
    id: usize,
}

///
/// Rename groups.
///
pub struct Renamer(BridgeRequest<Renamer>);
add_wrapper_new!(Renamer, BridgeRequest);
impl Renamer {
    pub async fn run(&mut self, id: usize, friendly_name: &str) -> Result<(), Error> {
        self.0
            .request(RenameRequest {
                from: id,
                to: friendly_name.to_owned(),
                homeassistant_rename: true,
            })
            .await?;
        Ok(())
    }
}
impl BridgeRequestType for Renamer {
    const NAME: &'static str = "group/rename";
    type Request = RenameRequest;
    type Response = RenameResponse;

    fn matches(request: &Self::Request, response: &RequestResponse<Self::Response>) -> bool {
        match response {
            RequestResponse::Ok { data } => data.to == request.to,
            RequestResponse::Error { error } => {
                error.contains(&format!("'{name}'", name = request.to))
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct RenameRequest {
    from: usize,
    to: String,
    homeassistant_rename: bool,
}
#[derive(Deserialize)]
pub(crate) struct RenameResponse {
    to: String,
}
