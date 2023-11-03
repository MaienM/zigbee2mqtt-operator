#![warn(missing_docs)]
#![warn(clippy::pedantic)]

//! An Kubernetes operator to manage resources on Zigbee2MQTT instances.

use std::{env, sync::Arc, time::Duration};

use async_trait::async_trait;
use error::ErrorWithMeta;
pub use event_manager::EventCore;
use k8s_openapi::api::core::v1::ObjectReference;
use kube::{core::object::HasStatus, runtime::controller::Action, Client, Resource};
use once_cell::sync::Lazy;
use reconcilers::instance::InstanceTracker;

pub mod crds;
pub mod error;
mod event_manager;
mod mqtt;
mod reconcilers;
mod status_manager;
mod sync_utils;
mod with_source;

static NAME: Lazy<String> = Lazy::new(|| env::var("HOSTNAME").unwrap_or("unknown".to_string()));

/// The timeout for responses/messages from Zigbee2MQTT.
static TIMEOUT: Lazy<Duration> = Lazy::new(|| {
    Duration::from_secs(env::var("Z2MOP_ACTION_TIMEOUT").map_or(5, |v| v.parse().unwrap()))
});

/// The maximum interval between reconciles of resources.
///
/// Changes to the K8S resource will immediately trigger a reconcile regardless of this interval, so this only dictates how quickly changes made on the Zigbee2MQTT side will be reconciled.
pub static RECONCILE_INTERVAL: Lazy<Duration> = Lazy::new(|| {
    Duration::from_secs(env::var("Z2MOP_RECONCILE_INTERVAL").map_or(60, |v| v.parse().unwrap()))
});

/// The maximum time to wait after a failed reconcile attempt before retrying.
///
/// Changes to the K8s resource will immediately trigger a reconcile, so this is only important for cases where the failure is temporary and the reconcile can succeed on a retry, not for cases where the resource is invalid in some way.
pub static RECONCILE_INTERVAL_FAILURE: Lazy<Duration> = Lazy::new(|| {
    Duration::from_secs(
        env::var("Z2MOP_RECONCILE_INTERVAL_FAILURE")
            .or(env::var("Z2MOP_RECONCILE_INTERVAL"))
            .map_or(15, |v| v.parse().unwrap()),
    )
});

/// The context that will be available to all reconcile actions.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client.
    pub client: Client,

    /// The current state.
    pub(crate) state: Arc<State>,
}
impl Context {
    /// Create new instance.
    #[allow(clippy::must_use_candidate)]
    pub fn new(client: Client) -> Self {
        Self {
            state: Arc::new(State {
                managers: InstanceTracker::new(client.clone()),
            }),
            client,
        }
    }
}

/// Shared state of the application.
pub(crate) struct State {
    /// The Zigbee2MQTT manager for each instance.
    pub(crate) managers: Arc<InstanceTracker>,
}

/// Reconcile actions for a CRD.
#[async_trait]
pub trait Reconciler: Resource + HasStatus + Sized {
    /// Handler for [`kube::runtime::finalizer::Event::Apply`].
    async fn reconcile(&self, ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta>;
    /// Handler for [`kube::runtime::finalizer::Event::Cleanup`].
    async fn cleanup(&self, ctx: &Arc<Context>) -> Result<Action, ErrorWithMeta>;
}

/// Wrap an async block that is expected to run forever.
///
/// If it doesn't the `$on_stop` block is called with an [`error::Error::InvariantFailed`].
macro_rules! background_task {
    ($name:expr, $run:block, $on_stop:block) => {{
        let run = $run;
        let on_stop = $on_stop;
        async move {
            let name = stringify!($name);
            let result: Result<(), Error> = run.await;
            let error = match result {
                Ok(_) => Error::InvariantFailed(format!("{name} closed"), None),
                Err(err) => Error::InvariantFailed(
                    format!("{name} encountered an error"),
                    Some(Box::new(err)),
                ),
            };
            on_stop(error).await;
        }
    }};
}
pub(crate) use background_task;

/// Utility functions for [`Resource`]s.
pub trait ResourceLocalExt {
    /// Generates an object reference for the resource.
    ///
    /// Like [`Resource::object_ref`], but easier to use.
    fn get_ref(&self) -> ObjectReference;
}
impl<T> ResourceLocalExt for T
where
    T: Resource,
    <T as Resource>::DynamicType: Default,
{
    fn get_ref(&self) -> ObjectReference {
        let dt = <Self as Resource>::DynamicType::default();
        self.object_ref(&dt)
    }
}

/// Utility functions for [`ObjectReference`]s.
pub trait ObjectReferenceLocalExt {
    /// Get the name including the namespace for this resource.
    fn full_name(&self) -> String;

    /// Get a string that uniquely represent this resouce in the form of `{api_version}/{kind}/{full_name}`.
    fn id(&self) -> String;
}
impl ObjectReferenceLocalExt for ObjectReference {
    fn full_name(&self) -> String {
        let name = self.name.as_ref().unwrap();
        match &self.namespace {
            Some(namespace) => format!("{namespace}/{name}"),
            None => name.clone(),
        }
    }

    fn id(&self) -> String {
        format!(
            "{api_version}/{kind}/{full_name}",
            api_version = self.api_version.as_ref().unwrap(),
            kind = self.kind.as_ref().unwrap(),
            full_name = self.full_name(),
        )
    }
}
