use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    super::manager::Manager,
    lib::{
        add_wrapper_new, setup_configuration_manager, BridgeRequest, BridgeRequestType,
        Configuration, ConfigurationManagerInner, RequestResponse, TopicTracker, TopicTrackerType,
    },
};
use crate::{
    error::{Error, ErrorWithMeta},
    mqtt::exposes::{GroupOptionsSchema, Processor},
    with_source::ValueWithSource,
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
    pub ieee_address: String,
}

///
/// Create groups.
///
pub struct Creator(BridgeRequest<Creator>);
add_wrapper_new!(Creator, BridgeRequest);
impl Creator {
    pub async fn run(
        &mut self,
        id: ValueWithSource<Option<usize>>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<usize, ErrorWithMeta> {
        let value = self.0.request(CreateRequest { id, friendly_name }).await?;
        Ok(value.id)
    }
}
impl BridgeRequestType for Creator {
    const NAME: &'static str = "group/add";
    type Request = CreateRequest;
    type Response = CreateResponse;

    fn process_response(
        request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>> {
        match response {
            RequestResponse::Ok { ref data } => {
                if data.friendly_name == *request.friendly_name {
                    Some(response.into())
                } else {
                    None
                }
            }
            RequestResponse::Error { ref error } => {
                if let Some(id) = *request.id {
                    if error.contains(&format!("'{id}'")) {
                        return Some(response.convert().map_err(|err| err.caused_by(&request.id)));
                    }
                }
                if error.contains(&format!("'{}'", request.friendly_name)) {
                    Some(
                        response
                            .convert()
                            .map_err(|err| err.caused_by(&request.friendly_name)),
                    )
                } else {
                    None
                }
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct CreateRequest {
    id: ValueWithSource<Option<usize>>,
    friendly_name: ValueWithSource<String>,
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
    pub async fn run(&mut self, id: ValueWithSource<usize>) -> Result<(), ErrorWithMeta> {
        self.0.request(DeleteRequest { id }).await?;
        Ok(())
    }
}
impl BridgeRequestType for Deletor {
    const NAME: &'static str = "group/remove";
    type Request = DeleteRequest;
    type Response = DeleteResponse;

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
                if error.contains(&format!(r#""groupid":{id}"#, id = request.id)) {
                    Some(response.convert().map_err(|err| err.caused_by(&request.id)))
                } else {
                    None
                }
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct DeleteRequest {
    id: ValueWithSource<usize>,
}
#[derive(Deserialize)]
pub(crate) struct DeleteResponse {
    id: usize,
}

///
/// Rename groups.
///
pub struct Renamer(BridgeRequest<Renamer>);
add_wrapper_new!(Renamer, BridgeRequest);
impl Renamer {
    pub async fn run(
        &mut self,
        id: ValueWithSource<usize>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        self.0
            .request(RenameRequest {
                from: id,
                to: friendly_name,
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
                if error.contains(&format!("'{}'", request.from)) {
                    Some(
                        response
                            .convert()
                            .map_err(|err| err.caused_by(&request.from)),
                    )
                } else if error.contains(&format!("'{}'", request.to)) {
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
    from: ValueWithSource<usize>,
    to: ValueWithSource<String>,
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
    pub async fn run(
        &mut self,
        group: ValueWithSource<usize>,
        device: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        self.0.request(MemberRequest { group, device }).await?;
        Ok(())
    }
}
impl BridgeRequestType for MemberAdder {
    const NAME: &'static str = "group/members/add";
    type Request = MemberRequest;
    type Response = MemberResponse;

    fn process_response(
        request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>> {
        match response {
            RequestResponse::Ok { ref data } => {
                if data.group == *request.group && data.device == *request.device {
                    Some(response.into())
                } else {
                    None
                }
            }
            RequestResponse::Error { ref error } => {
                if error.contains(&format!("'{}'", request.group)) {
                    Some(
                        response
                            .convert()
                            .map_err(|err| err.caused_by(&request.group)),
                    )
                } else if error.contains(&format!("'{}'", request.device)) {
                    Some(
                        response
                            .convert()
                            .map_err(|err| err.caused_by(&request.device)),
                    )
                } else {
                    None
                }
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct MemberRequest {
    group: ValueWithSource<usize>,
    device: ValueWithSource<String>,
}
#[derive(Deserialize)]
pub(crate) struct MemberResponse {
    group: usize,
    device: String,
}

///
/// Remove a device from a group.
///
pub struct MemberRemover(BridgeRequest<MemberRemover>);
add_wrapper_new!(MemberRemover, BridgeRequest);
impl MemberRemover {
    pub async fn run(
        &mut self,
        group: ValueWithSource<usize>,
        device: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        self.0.request(MemberRequest { group, device }).await?;
        Ok(())
    }
}
impl BridgeRequestType for MemberRemover {
    const NAME: &'static str = "group/members/remove";
    type Request = <MemberAdder as BridgeRequestType>::Request;
    type Response = <MemberAdder as BridgeRequestType>::Response;

    fn process_response(
        request: &Self::Request,
        response: RequestResponse<Self::Response>,
    ) -> Option<Result<Self::Response, ErrorWithMeta>> {
        <MemberAdder as BridgeRequestType>::process_response(request, response)
    }
}

///
/// Manage group options.
///
pub struct OptionsManager {
    manager: Arc<Manager>,
    id: ValueWithSource<usize>,
}
impl OptionsManager {
    pub fn new(manager: Arc<Manager>, id: ValueWithSource<usize>) -> Self {
        Self { manager, id }
    }
}
setup_configuration_manager!(OptionsManager);
#[async_trait]
impl ConfigurationManagerInner for OptionsManager {
    const NAME: &'static str = "option";

    async fn schema(&mut self) -> Result<Box<dyn Processor + Send + Sync>, ErrorWithMeta> {
        Ok(Box::<GroupOptionsSchema>::default())
    }

    async fn get(&mut self) -> Result<Configuration, ErrorWithMeta> {
        Ok(self
            .manager
            .get_bridge_info_tracker()
            .await?
            .get()
            .await?
            .config
            .groups
            .get(&self.id)
            .cloned()
            .unwrap_or_default())
    }

    async fn set(
        &mut self,
        configuration: &ValueWithSource<Configuration>,
    ) -> Result<Configuration, ErrorWithMeta> {
        let mut setter = OptionsSetter(BridgeRequest::new(self.manager.clone()).await?);
        let value = setter
            .0
            .request(OptionsSetRequest {
                id: self.id.clone(),
                options: configuration.clone(),
            })
            .await?;
        Ok(value.to)
    }

    async fn unset(
        &mut self,
        paths: ValueWithSource<Vec<String>>,
    ) -> Result<Configuration, ErrorWithMeta> {
        let mut setter = OptionsUnsetter(BridgeRequest::new(self.manager.clone()).await?);
        let value = setter
            .0
            .request(OptionsUnsetRequest {
                id: self.id.clone(),
                paths,
            })
            .await?;
        Ok(value.to)
    }

    fn is_property_clearable(key: &str) -> bool {
        !matches!(key, "ID" | "friendly_name" | "devices")
    }
}

pub struct OptionsSetter(BridgeRequest<OptionsSetter>);
impl BridgeRequestType for OptionsSetter {
    const NAME: &'static str = "group/options";
    type Request = OptionsSetRequest;
    type Response = OptionsSetResponse;

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
                if error.contains(&format!("'{}'", request.id)) {
                    Some(response.convert().map_err(|err| err.caused_by(&request.id)))
                } else {
                    None
                }
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct OptionsSetRequest {
    id: ValueWithSource<usize>,
    options: ValueWithSource<Configuration>,
}
#[derive(Deserialize)]
pub(crate) struct OptionsSetResponse {
    id: usize,
    to: Configuration,
}

pub struct OptionsUnsetter(BridgeRequest<OptionsUnsetter>);
impl BridgeRequestType for OptionsUnsetter {
    const NAME: &'static str = "group/unset-options";
    type Request = OptionsUnsetRequest;
    type Response = OptionsUnsetResponse;

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
                if error.contains(&format!("'{}'", request.id)) {
                    Some(response.convert().map_err(|err| err.caused_by(&request.id)))
                } else {
                    None
                }
            }
        }
    }
}
#[derive(Serialize, Debug, Clone)]
pub(crate) struct OptionsUnsetRequest {
    id: ValueWithSource<usize>,
    paths: ValueWithSource<Vec<String>>,
}
#[derive(Deserialize)]
pub(crate) struct OptionsUnsetResponse {
    id: usize,
    to: Configuration,
}
