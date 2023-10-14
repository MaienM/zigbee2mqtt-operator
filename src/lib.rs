use std::{collections::HashMap, env, fmt::Debug, sync::Arc, time::Duration};

use async_trait::async_trait;
use event_manager::{EventCore, EventManager, EventType};
use kube::{
    core::object::HasStatus,
    runtime::{controller::Action, finalizer::Error as FinalizerError},
    Client, Resource,
};
use mqtt::MQTTManager;
use once_cell::sync::Lazy;
use thiserror::Error;
use tokio::sync::Mutex;

pub mod crds;
pub mod event_manager;
pub mod ext;
mod instance;
mod mqtt;
pub mod status_manager;
mod sync_utils;

static NAME: Lazy<String> = Lazy::new(|| env::var("HOSTNAME").unwrap_or("unknown".to_string()));
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

    /// A misconfiguration of a Kubernetes resource. This is only for problems that require a change to a resource to resolve, not for temporary failures that might resolve by themselves.
    #[error("Invalid resource: {0}")]
    InvalidResource(String),

    /// The MQTT broker responded in an unexpected manner.
    #[error("MQTT error: {0}{err}", err = maybe!(.1))]
    MQTTError(
        String,
        #[source] Option<Arc<Box<dyn std::error::Error + Send + Sync>>>,
    ),

    /// Something in the process of reading and processing messages for a subscription failed.
    #[error("Subscription error on topic {topic}: {message}{err}", err = maybe!(.source))]
    SubscriptionError {
        topic: String,
        message: String,
        #[source]
        source: Option<Arc<Box<dyn std::error::Error + Send + Sync>>>,
    },

    /// A manager has been shut down and should no longer be used.
    #[error("Manager has been shut down{err}", err = maybe!(.0))]
    ManagerShutDown(Option<Box<Error>>),

    /// Some invariant failed. This probably indicates a bug, and it will cause the affected subsystem to be restarted to get back to a known state.
    #[error("Invariant failed: {0}{err}", err = maybe!(.1))]
    InvariantFailed(String, Option<Box<Error>>),

    /// Something prevented the requested action from completing successfully. This is the catch-all for all errors that don't fit any of the more specific types.
    #[error("Action failed: {0}")]
    ActionFailed(
        String,
        #[source] Option<Arc<Box<dyn std::error::Error + Send + Sync>>>,
    ),
}

/// An [`Error`] for which an [`Event`] has already been published.
pub struct EmittedError(pub Error);
#[async_trait]
pub trait EmittableResult<V> {
    /// Publish error.
    async fn emit_event<F>(self, manager: &EventManager) -> Result<V, EmittedError>;

    /// Shorthand version of [`EmittableResult::emit_event_map`] which sets only [`EventCore::field_path`].
    async fn emit_event_with_path(
        self,
        manager: &EventManager,
        field_path: &str,
    ) -> Result<V, EmittedError>;

    /// Publish error, allowing closure to alter EventCore beforehand. See [`EventCore::field_path`].
    async fn emit_event_map<F>(self, manager: &EventManager, f: F) -> Result<V, EmittedError>
    where
        F: FnOnce(&mut EventCore) -> () + Send;
}
#[async_trait]
impl<V> EmittableResult<V> for Result<V, Error>
where
    V: Send,
{
    async fn emit_event<F>(self, manager: &EventManager) -> Result<V, EmittedError> {
        self.emit_event_map(manager, |_| {}).await
    }

    async fn emit_event_with_path(
        self,
        manager: &EventManager,
        field_path: &str,
    ) -> Result<V, EmittedError> {
        self.emit_event_map(manager, |event| {
            event.field_path = Some(field_path.to_string());
        })
        .await
    }

    async fn emit_event_map<F>(self, manager: &EventManager, f: F) -> Result<V, EmittedError>
    where
        F: FnOnce(&mut EventCore) -> () + Send,
    {
        if let Err(ref error) = self {
            let mut event = EventCore {
                action: "Reconciling".to_string(),
                note: Some(error.to_string()),
                reason: "Created".to_string(),
                type_: EventType::Warning,
                ..EventCore::default()
            };
            f(&mut event);
            manager.publish_nolog(event).await;
        }
        self.map_err(EmittedError)
    }
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
pub trait Reconciler: Resource + HasStatus + Sized {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, EmittedError>;
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, EmittedError>;
}

macro_rules! background_task {
    ($name:expr, $run:block, $on_stop:block) => {{
        let run = $run;
        let on_stop = $on_stop;
        async move {
            let name = stringify!($name);
            let result: Result<(), Error> = run.await;
            let error = match result {
                Ok(_) => Error::InvariantFailed(format!("{name} closed"), None),
                Err(err) => {
                    Error::InvariantFailed(format!("{name} encountered error"), Some(Box::new(err)))
                }
            };
            on_stop(error).await;
        }
    }};
}
pub(crate) use background_task;
