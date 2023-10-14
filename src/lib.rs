use std::sync::Arc;

use async_trait::async_trait;
use kube::{
    runtime::{controller::Action, finalizer::Error as FinalizerError},
    Client, Resource,
};
use thiserror::Error;

pub mod crds;
pub mod ext;
mod instance;

#[derive(Error, Debug, Clone)]
pub enum Error {
    /// Wrapped errors for a failed reconcile action.
    #[error("Finalizer error: {0}")]
    FinalizerError(#[source] Arc<Box<FinalizerError<Error>>>),
}

#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// State.
    pub state: Arc<State>,
}

#[derive(Default)]
pub struct State {}

#[async_trait]
pub trait Reconciler: Resource + Sized {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, Error>;
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, Error>;
}
