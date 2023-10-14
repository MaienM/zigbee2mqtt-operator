use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use kube::runtime::controller::Action;
use tracing::info_span;

use crate::{crds::Instance, Context, Error, Reconciler};

#[async_trait]
impl Reconciler for Instance {
    async fn reconcile(&self, _ctx: Arc<Context>) -> Result<Action, Error> {
        info_span!("reconcile", ?self);

        return Ok(Action::requeue(Duration::from_secs(15)));
    }

    async fn cleanup(&self, _ctx: Arc<Context>) -> Result<Action, Error> {
        info_span!("cleanup", ?self);

        return Ok(Action::requeue(Duration::from_secs(5 * 60)));
    }
}
