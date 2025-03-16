//! Utility to manage the status of Kubernetes resources.

use std::sync::Arc;

use futures::Future;
use k8s_openapi::NamespaceResourceScope;
use kube::{
    api::{Patch, PatchParams},
    core::object::HasStatus,
    Api, Client, CustomResourceExt, Resource, ResourceExt,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use tokio::spawn;
use tracing::error_span;

use crate::{ObjectReferenceLocalExt, ResourceLocalExt, NAME};

#[derive(Clone)]
pub struct StatusManager<T>
where
    T: Resource<Scope = NamespaceResourceScope>
        + CustomResourceExt
        + HasStatus
        + DeserializeOwned
        + 'static,
    <T as Resource>::DynamicType: Default,
    <T as HasStatus>::Status: Default + Clone + Serialize + PartialEq,
{
    api: Arc<Api<T>>,
    id: String,
    name: String,
    old: Option<T::Status>,
    current: T::Status,
}
impl<T> StatusManager<T>
where
    T: Resource<Scope = NamespaceResourceScope> + CustomResourceExt + HasStatus + DeserializeOwned,
    <T as Resource>::DynamicType: Default,
    <T as HasStatus>::Status: Default + Clone + Serialize + PartialEq,
{
    pub fn new(client: Client, resource: &T) -> Self {
        Self {
            api: Arc::new(Api::namespaced(client, &resource.namespace().unwrap())),
            id: resource.get_ref().id(),
            name: resource.name_any(),
            old: resource.status().cloned(),
            current: resource
                .status()
                .map_or_else(T::Status::default, T::Status::clone),
        }
    }

    pub fn get(&self) -> &T::Status {
        &self.current
    }

    pub fn set(&mut self, status: T::Status) {
        self.current = status;
    }

    pub fn update<F>(&mut self, mut func: F)
    where
        F: FnMut(&mut T::Status),
    {
        let mut status = self.current.clone();
        func(&mut status);
        self.set(status);
    }

    fn do_sync(&mut self) -> impl Future<Output = ()> {
        let api = self.api.clone();
        let id = self.id.clone();
        let name = self.name.clone();

        let patch_params = PatchParams::apply(&NAME).force();
        let api_resource = T::api_resource();
        let patch = Patch::Apply(json!({
            "apiVersion": api_resource.api_version,
            "kind": api_resource.kind,
            "status": self.current,
        }));

        async move {
            let result = api.patch_status(&name, &patch_params, &patch).await;
            if let Err(err) = result {
                error_span!("failed to update resource status", id, ?err);
            }
        }
    }

    pub async fn sync(&mut self) {
        if Some(self.current.clone()) == self.old {
            return;
        }
        self.old = Some(self.current.clone());

        self.do_sync().await;
    }
}
impl<T> Drop for StatusManager<T>
where
    T: Resource<Scope = NamespaceResourceScope>
        + CustomResourceExt
        + HasStatus
        + DeserializeOwned
        + 'static,
    <T as Resource>::DynamicType: Default,
    <T as HasStatus>::Status: Default + Clone + Serialize + PartialEq,
{
    fn drop(&mut self) {
        if Some(self.current.clone()) != self.old {
            spawn(self.do_sync());
        }
    }
}
