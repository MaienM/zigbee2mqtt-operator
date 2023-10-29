//! Errors & utilities to manage them.

use std::sync::Arc;

use async_trait::async_trait;
use futures::Future;
use kube::runtime::finalizer::Error as FinalizerError;
use thiserror::Error;

use crate::event_manager::{EventCore, EventManager, EventType};

macro_rules! maybe {
    ($value:expr) => {
        $value
            .clone()
            .map_or("".to_string(), |value| format!(": {:?}", value))
    };
}

/// The error type of this crate.
///
/// Any error that needs to propagate up needs to be of this type, and any error generated by another crate that isn't handled needs to be wrapped in this type.
#[derive(Error, Debug, Clone)]
pub enum Error {
    /// Wrapped errors for a failed reconcile action.
    #[error("Finalizer error: {0}")]
    FinalizerError(#[source] Arc<Box<FinalizerError<Error>>>),

    /// A misconfiguration of a Kubernetes resource. This is only for problems that require a change to a resource to resolve, not for temporary failures that might resolve by themselves.
    #[error("Invalid resource at {field_path}: {message}")]
    InvalidResource {
        /// The path of the portion of the resource that is problematic, like [`EventCore::field_path`].
        ///
        /// This should be a partial JSON path that can be appended directly to another path (e.g. `.foo` or `[12]`) so that the full path can be emitted easily with [`EmittableResult::emit_event_with_path`].
        field_path: String,
        /// A human-readable message describing the problem with the value.
        message: String,
    },

    /// The MQTT broker responded in an unexpected manner.
    #[error("MQTT error: {0}{err}", err = maybe!(.1))]
    MQTTError(
        String,
        #[source] Option<Arc<Box<dyn std::error::Error + Send + Sync>>>,
    ),

    /// The Zigbee2MQTT application responded in an unexpected manner.
    #[error("Zigbee2MQTT error: {0}")]
    Zigbee2MQTTError(String),

    /// Something in the process of reading and processing messages for a subscription failed.
    #[error("Subscription error on topic {topic}: {message}{err}", err = maybe!(.source))]
    SubscriptionError {
        /// The topic of the subscription that generated this error.
        topic: String,
        /// A human-readable message describing the issue.
        message: String,
        /// The underlying error that caused this issue, if any.
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

/// An [`enum@Error`] for which an [`k8s_openapi::api::events::v1::Event`] has already been published.
///
/// Created with [`EmittableResult`].
#[allow(clippy::module_name_repetitions)]
pub struct EmittedError(pub Error);

/// Helper to emit an [`enum@Error`] contained inside a [`Result`], converting it to an [`EmittedError`] in the process.
#[async_trait]
pub trait EmittableResult<V> {
    /// Emit the event, using the given path as context for the source of the error (see [`EventCore::field_path`]).
    ///
    /// For [`Error::InvalidResource`] the paths will be combined as `{field_path}{error.field_path}`.
    async fn emit_event(self, manager: &EventManager, field_path: &str) -> Result<V, EmittedError>;

    /// Emit the event.
    async fn emit_event_nopath(self, manager: &EventManager) -> Result<V, EmittedError>;

    /// Mark error as published without actually publishing anything. This should only be used in cases where one or more events have already been published for the error manually.
    fn fake_emit_event(self) -> Result<V, EmittedError>;
}
#[async_trait]
impl<V> EmittableResult<V> for Result<V, Error>
where
    V: Send,
{
    async fn emit_event(
        mut self,
        manager: &EventManager,
        field_path: &str,
    ) -> Result<V, EmittedError> {
        // Special case for Error::InvalidResource where we combine the two paths and use the result for both the InvalidResource path and the EventCore path.
        let field_path = match self {
            Err(Error::InvalidResource {
                field_path: ref mut error_field_path,
                ..
            }) => {
                *error_field_path = format!("{field_path}{error_field_path}");
                error_field_path.clone()
            }
            _ => field_path.to_owned(),
        };

        if let Err(ref error) = self {
            manager
                .publish_nolog(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some(error.to_string()),
                    reason: "Created".to_string(),
                    type_: EventType::Warning,
                    field_path: Some(field_path),
                })
                .await;
        }
        self.map_err(EmittedError)
    }

    async fn emit_event_nopath(mut self, manager: &EventManager) -> Result<V, EmittedError> {
        if let Err(ref error) = self {
            manager
                .publish_nolog(EventCore {
                    action: "Reconciling".to_string(),
                    note: Some(error.to_string()),
                    reason: "Created".to_string(),
                    type_: EventType::Warning,
                    field_path: None,
                })
                .await;
        }
        self.map_err(EmittedError)
    }

    fn fake_emit_event(self) -> Result<V, EmittedError> {
        self.map_err(EmittedError)
    }
}

/// Helper to use [`EmittableResult`] methods directly on a [`Future`], saving a layer of `.await` calls.
#[async_trait]
pub trait EmittableResultFuture<V> {
    /// See [`EmittableResult::emit_event`].
    async fn emit_event(self, manager: &EventManager, field_path: &str) -> Result<V, EmittedError>;

    /// See [`EmittableResult::emit_event_nopath`].
    async fn emit_event_nopath(self, manager: &EventManager) -> Result<V, EmittedError>;

    /// See [`EmittableResult::fake_emit_event`].
    async fn fake_emit_event(self) -> Result<V, EmittedError>;
}
#[async_trait]
impl<F, V> EmittableResultFuture<V> for F
where
    F: Future<Output = Result<V, Error>> + Send,
    V: Send,
{
    async fn emit_event(
        mut self,
        manager: &EventManager,
        field_path: &str,
    ) -> Result<V, EmittedError> {
        self.await.emit_event(manager, field_path).await
    }

    async fn emit_event_nopath(mut self, manager: &EventManager) -> Result<V, EmittedError> {
        self.await.emit_event_nopath(manager).await
    }

    async fn fake_emit_event(self) -> Result<V, EmittedError> {
        self.await.fake_emit_event()
    }
}
