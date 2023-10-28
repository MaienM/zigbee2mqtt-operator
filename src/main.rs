use std::{any::type_name, fmt::Debug, hash::Hash, sync::Arc, time::Duration};

use futures::{
    future::{self},
    StreamExt,
};
use k8s_openapi::NamespaceResourceScope;
use kube::{
    api::ListParams,
    core::object::HasStatus,
    runtime::{
        controller::Action,
        finalizer::{finalizer, Event as Finalizer},
        watcher::Config,
        Controller,
    },
    Api, Client, CustomResourceExt, Resource, ResourceExt,
};
use serde::{de::DeserializeOwned, Serialize};
use tokio::join;
use tracing::{error_span, info_span};
use zigbee2mqtt_operator::{
    crds::{Device, Group, Instance},
    error::Error,
    Context, Reconciler, ResourceLocalExt, State,
};

pub static FINALIZER: &str = "zigbee2mqtt.maienm.com";

async fn reconcile<T>(resource: Arc<T>, ctx: Arc<Context>) -> Result<Action, Error>
where
    T: Resource
        + Clone
        + CustomResourceExt
        + Debug
        + DeserializeOwned
        + HasStatus
        + Reconciler
        + Serialize,
    T: Resource<Scope = NamespaceResourceScope>,
    <T as HasStatus>::Status: Default + Clone + Serialize + PartialEq,
    <T as Resource>::DynamicType: Default,
{
    let ns = resource.namespace().unwrap();
    let api: Api<T> = Api::namespaced(ctx.client.clone(), &ns);

    finalizer(&api, FINALIZER, resource, |event| async {
        match event {
            Finalizer::Apply(doc) => {
                let result = doc.reconcile(ctx.clone()).await;
                result.map_err(|err| err.0)
            }
            Finalizer::Cleanup(doc) => {
                let result = doc.cleanup(ctx.clone()).await;
                result.map_err(|err| err.0)
            }
        }
    })
    .await
    .map_err(|err| Error::FinalizerError(Arc::new(Box::new(err))))
}

fn error_policy<T>(resource: Arc<T>, error: &Error, _ctx: Arc<Context>) -> Action
where
    T: Resource + ResourceLocalExt + Reconciler + Clone + Serialize + DeserializeOwned + Debug,
    <T as Resource>::DynamicType: Default,
{
    error_span!(
        "reconcile failed",
        id = resource.id(),
        err = ?error,
    );
    Action::requeue(Duration::from_secs(60))
}

async fn start_controller<T>(client: Client, ctx: Arc<Context>)
where
    T: Resource
        + Clone
        + CustomResourceExt
        + Debug
        + DeserializeOwned
        + HasStatus
        + Reconciler
        + Send
        + Serialize
        + Sync,
    T: Resource<Scope = NamespaceResourceScope>,
    <T as HasStatus>::Status: Default + Clone + Serialize + PartialEq + Send,
    <T as Resource>::DynamicType: Default + Eq + Hash + Clone + Debug + Unpin,
{
    let api = Api::<T>::all(client);
    if let Err(err) = api.list(&ListParams::default().limit(1)).await {
        let kind = type_name::<T>();
        error_span!("unable to get resources, are the CRDs installed?", kind, err = ?err);
        info_span!("run `cargo run --bin crdgen` to generate CRDs YAMLs");
        std::process::exit(1);
    }

    Controller::new(api, Config::default().any_semantic())
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| future::ready(()))
        .await;
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let client = Client::try_default().await.unwrap();

    let ctx = Arc::new(Context {
        client: client.clone(),
        state: Arc::new(State::default()),
    });

    join!(
        start_controller::<Instance>(client.clone(), ctx.clone()),
        start_controller::<Device>(client.clone(), ctx.clone()),
        start_controller::<Group>(client.clone(), ctx.clone()),
    );
}
