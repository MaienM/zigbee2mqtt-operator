use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use kube::{
    runtime::{controller::Action, finalizer::Error as FinalizerError},
    Client, Resource,
};
use mqtt::MQTTManager;
use thiserror::Error;
use tokio::sync::Mutex;

pub mod crds;
pub mod ext;
mod instance;
mod mqtt;

const TIMEOUT: Duration = Duration::from_secs(5);

macro_rules! maybe {
    ($value:expr) => {
        $value
            .clone()
            .map_or("".to_string(), |value| format!(": {:?}", value))
    };
}

#[derive(Error, Debug, Clone)]
pub enum Error {
    /// Wrapped errors for a failed reconcile action.
    #[error("Finalizer error: {0}")]
    FinalizerError(#[source] Arc<Box<FinalizerError<Error>>>),

    /// The MQTT broker responded in an unexpected manner.
    #[error("MQTT error: {0}{err}", err = maybe!(.1))]
    MQTTError(
        String,
        #[source] Option<Arc<Box<dyn std::error::Error + Send + Sync>>>,
    ),

    /// A manager has been shut down and should no longer be used.
    #[error("Manager has been shut down{err}", err = maybe!(.0))]
    ManagerShutDown(Option<Box<Error>>),

    /// Something prevented the requested action from completing successfully. This is the catch-all for all errors that don't fit any of the more specific types.
    #[error("Action failed: {0}")]
    ActionFailed(
        String,
        #[source] Option<Arc<Box<dyn std::error::Error + Send + Sync>>>,
    ),
}

#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// State.
    pub state: Arc<State>,
}

#[derive(Default)]
pub struct State {
    /// The manager for each instance.
    pub managers: Mutex<HashMap<String, Arc<MQTTManager>>>,
}

#[async_trait]
pub trait Reconciler: Resource + Sized {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, Error>;
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, Error>;
}
