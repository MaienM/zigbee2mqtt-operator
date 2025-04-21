//! Utility to manage Kubernetes [`Event`]s.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use k8s_openapi::{
    api::{
        core::v1::ObjectReference,
        events::v1::{Event, EventSeries},
    },
    apimachinery::pkg::apis::meta::v1::MicroTime,
    chrono::Utc,
};
use kube::{api::PostParams, core::ObjectMeta, Api, Client};
use serde::Serialize;
use strum_macros::Display;
use tracing::{error_span, info_span};

use crate::{ObjectReferenceLocalExt, NAME};

pub trait EventExt {
    fn create(regarding: ObjectReference, core: EventCore) -> Self;
}
impl EventExt for Event {
    fn create(mut regarding: ObjectReference, core: EventCore) -> Self {
        regarding.field_path = core.field_path;

        let mut hasher = DefaultHasher::new();
        regarding.api_version.hash(&mut hasher);
        regarding.namespace.hash(&mut hasher);
        regarding.name.hash(&mut hasher);
        regarding.field_path.hash(&mut hasher);
        core.action.hash(&mut hasher);
        core.note.hash(&mut hasher);
        core.reason.hash(&mut hasher);
        core.type_.hash(&mut hasher);
        let hash = hasher.finish();

        let name = format!(
            "zigbee2mqtt-{}-{:x}",
            regarding.kind.clone().unwrap().to_lowercase(),
            hash,
        );
        Event {
            metadata: ObjectMeta {
                annotations: None,
                creation_timestamp: None,
                deletion_grace_period_seconds: None,
                deletion_timestamp: None,
                finalizers: None,
                generate_name: None,
                generation: None,
                labels: None,
                managed_fields: None,
                name: Some(name),
                namespace: regarding.namespace.clone(),
                owner_references: None,
                resource_version: None,
                self_link: None,
                uid: None,
            },
            action: Some(core.action.to_string()),
            deprecated_count: None,
            deprecated_first_timestamp: None,
            deprecated_last_timestamp: None,
            deprecated_source: None,
            event_time: Some(MicroTime(Utc::now())),
            note: core.note,
            reason: Some(core.reason.to_string()),
            regarding: Some(regarding),
            related: None,
            reporting_controller: Some("zigbee2mqtt-operator".into()),
            reporting_instance: Some(NAME.clone()),
            series: None,
            type_: Some(core.type_.to_string()),
        }
    }
}

/// Subset of a k8s event containing only the fields that a state change is expected to provide.
#[derive(Clone, Hash, Eq, PartialEq, Debug, Default)]
pub struct EventCore {
    /// What action was taken/failed. Machine-readable, at most 128 characters.
    pub action: String,

    /// Description of the status of this operation. human-readable, at most 1kB, optional.
    pub note: Option<String>,

    /// Why the action was taken. Human-readable, at most 128 characters.
    /// It looks like this is usually a single `PascalCase` word.
    pub reason: String,

    /// The type of this event.
    pub type_: EventType,

    /// As [`ObjectReference::field_path`].
    pub field_path: Option<String>,
}
#[derive(Clone, Display, Hash, Eq, PartialEq, Debug, Serialize, Default)]
pub enum EventType {
    #[default]
    Normal,
    Warning,
}

#[derive(Clone)]
pub struct EventManager {
    api: Api<Event>,
    regarding: ObjectReference,
}
impl EventManager {
    pub fn new(client: Client, regarding: ObjectReference) -> Self {
        Self {
            api: Api::namespaced(client, &regarding.namespace.clone().unwrap()),
            regarding,
        }
    }

    pub async fn publish(&self, core: EventCore) {
        info_span!("registering event", id = ?self.regarding.id(), ?core);
        self.publish_nolog(core).await;
    }

    pub async fn publish_nolog(&self, core: EventCore) {
        let new_event = Event::create(self.regarding.clone(), core.clone());

        let name = new_event.metadata.name.clone().unwrap();
        let entry = match self.api.entry(&name).await {
            Ok(entry) => entry,
            Err(err) => {
                error_span!("failed to query for existing event", regarding = ?self.regarding, ?core, ?err);
                return;
            }
        };

        let result = entry
            .and_modify(|event| match event.series {
                Some(ref mut series) => {
                    series.count += 1;
                    series.last_observed_time = new_event.event_time.clone().unwrap();
                }
                None => {
                    event.series = Some(EventSeries {
                        count: 2,
                        last_observed_time: new_event.event_time.clone().unwrap(),
                    });
                }
            })
            .or_insert(|| new_event)
            .commit(&PostParams::default())
            .await;
        if let Err(err) = result {
            error_span!("failed to publish event", regarding = ?self.regarding, ?core, ?err);
        }
    }
}
