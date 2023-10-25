#![warn(missing_docs)]
#![warn(clippy::pedantic)]

//! An Kubernetes operator to manage resources on Zigbee2MQTT instances.

use std::{env, sync::Arc, time::Duration};

use async_trait::async_trait;
use error::EmittedError;
pub use event_manager::EventCore;
use kube::{core::object::HasStatus, runtime::controller::Action, Client, Resource, ResourceExt};
use mqtt::Manager;
use once_cell::sync::Lazy;
use sync_utils::AwaitableMap;

pub mod crds;
pub mod error;
mod event_manager;
mod mqtt;
mod reconcilers;
mod status_manager;
mod sync_utils;

static NAME: Lazy<String> = Lazy::new(|| env::var("HOSTNAME").unwrap_or("unknown".to_string()));
const TIMEOUT: Duration = Duration::from_secs(5);

/// The context that will be available to all reconcile actions.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client.
    pub client: Client,
    /// The current state.
    pub state: Arc<State>,
}

/// Shared state of the application.
pub struct State {
    /// The Zigbee2MQTT manager for each instance.
    pub managers: AwaitableMap<Arc<Manager>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            managers: AwaitableMap::new("instance manager".to_string()),
        }
    }
}

/// Reconcile actions for a CRD.
#[async_trait]
pub trait Reconciler: Resource + HasStatus + Sized {
    /// Handler for [`kube::runtime::finalizer::Event::Apply`].
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, EmittedError>;
    /// Handler for [`kube::runtime::finalizer::Event::Cleanup`].
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, EmittedError>;
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
    /// Get the name including the namespace for this resource.
    fn full_name(&self) -> String;
    /// Get a string that uniquely represent this resouce in the form of `{api_version}/{kind}/{full_name}`.
    fn id(&self) -> String;
}
impl<T> ResourceLocalExt for T
where
    T: Resource,
    <T as Resource>::DynamicType: Default,
{
    fn full_name(&self) -> String {
        let name = self.name_any();
        match self.namespace() {
            Some(namespace) => format!("{namespace}/{name}"),
            None => name,
        }
    }

    fn id(&self) -> String {
        let dt = <Self as Resource>::DynamicType::default();
        format!(
            "{api_version}/{kind}/{full_name}",
            api_version = Self::api_version(&dt),
            kind = Self::kind(&dt),
            full_name = self.full_name(),
        )
    }
}
