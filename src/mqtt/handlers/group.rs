use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    super::manager::Manager,
    lib::{
        add_wrapper_new, setup_configuration_manager, BridgeRequest, BridgeRequestType,
        Configuration, ConfigurationManager, ConfigurationManagerInner, RequestResponse,
        TopicTracker, TopicTrackerType,
    },
};
use crate::{
    error::{EmittableResultFuture, EmittedError, Error},
    event_manager::EventManager,
    mqtt::exposes::{GroupOptionsSchema, Processor},
};

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
    pub async fn get_all(&self) -> Result<Vec<BridgeGroup>, Error> {
        self.0.get().await
    }

    pub async fn get_by_id(&self, id: usize) -> Result<Option<BridgeGroup>, Error> {
        Ok(self.get_all().await?.into_iter().find(|g| g.id == id))
    }

    pub async fn get_by_friendly_name(
        &self,
        friendly_name: &str,
    ) -> Result<Option<BridgeGroup>, Error> {
        Ok(self
            .get_all()
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

///
/// Add a device to a group.
///
pub struct MemberAdder(BridgeRequest<MemberAdder>);
add_wrapper_new!(MemberAdder, BridgeRequest);
impl MemberAdder {
    pub async fn run(&mut self, group: usize, device: &str) -> Result<(), Error> {
        self.0
            .request(MemberPayload {
                group,
                device: device.to_owned(),
            })
            .await?;
        Ok(())
    }
}
impl BridgeRequestType for MemberAdder {
    const NAME: &'static str = "group/members/add";
    type Request = MemberPayload;
    type Response = MemberPayload;

    fn matches(request: &Self::Request, response: &RequestResponse<Self::Response>) -> bool {
        match response {
            RequestResponse::Ok { data } => data == request,
            RequestResponse::Error { error } => {
                error.contains(&format!("'{group}'", group = request.group))
                    || error.contains(&format!("'{device}'", device = request.device))
            }
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct MemberPayload {
    group: usize,
    device: String,
}

///
/// Remove a device from a group.
///
pub struct MemberRemover(BridgeRequest<MemberRemover>);
add_wrapper_new!(MemberRemover, BridgeRequest);
impl MemberRemover {
    pub async fn run(&mut self, group: usize, device: &str) -> Result<(), Error> {
        self.0
            .request(MemberPayload {
                group,
                device: device.to_owned(),
            })
            .await?;
        Ok(())
    }
}
impl BridgeRequestType for MemberRemover {
    const NAME: &'static str = "group/members/remove";
    type Request = <MemberAdder as BridgeRequestType>::Request;
    type Response = <MemberAdder as BridgeRequestType>::Response;

    fn matches(request: &Self::Request, response: &RequestResponse<Self::Response>) -> bool {
        <MemberAdder as BridgeRequestType>::matches(request, response)
    }
}

///
/// Manage group options.
///
pub struct OptionsManager {
    manager: Arc<Manager>,
    request_manager: BridgeRequest<OptionsManager>,
    id: usize,
}
impl OptionsManager {
    pub async fn new(manager: Arc<Manager>, id: usize) -> Result<Self, Error> {
        Ok(Self {
            manager: manager.clone(),
            request_manager: BridgeRequest::new(manager).await?,
            id,
        })
    }
}
setup_configuration_manager!(OptionsManager);
#[async_trait]
impl ConfigurationManagerInner for OptionsManager {
    const NAME: &'static str = "option";
    const PATH: &'static str = "spec.options";

    async fn schema(
        &mut self,
        _eventmanager: &EventManager,
    ) -> Result<Box<dyn Processor + Send + Sync>, EmittedError> {
        Ok(Box::<GroupOptionsSchema>::default())
    }

    async fn get(&mut self, eventmanager: &EventManager) -> Result<Configuration, EmittedError> {
        Ok(self
            .manager
            .get_bridge_info_tracker()
            .emit_event_nopath(eventmanager)
            .await?
            .get()
            .emit_event_nopath(eventmanager)
            .await?
            .config
            .groups
            .get(&self.id)
            .cloned()
            .unwrap_or_default())
    }

    async fn set(&mut self, configuration: &Configuration) -> Result<Configuration, Error> {
        let value = self
            .request_manager
            .request(OptionsRequest {
                id: self.id,
                options: configuration.clone(),
            })
            .await?;
        Ok(value.to)
    }

    fn clear_property(key: &str) -> bool {
        !matches!(key, "friendly_name" | "devices")
    }
}
impl BridgeRequestType for OptionsManager {
    const NAME: &'static str = "group/options";
    type Request = OptionsRequest;
    type Response = OptionsResponse;

    fn matches(request: &Self::Request, response: &RequestResponse<Self::Response>) -> bool {
        match response {
            RequestResponse::Ok { data } => data.id == request.id,
            RequestResponse::Error { error } => {
                error.contains(&format!("'{ieee_address}'", ieee_address = request.id))
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct OptionsRequest {
    id: usize,
    options: Configuration,
}
#[derive(Deserialize)]
pub(crate) struct OptionsResponse {
    id: usize,
    to: Configuration,
}
