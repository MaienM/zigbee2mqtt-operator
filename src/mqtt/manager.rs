use std::{collections::HashMap, fmt::Debug, sync::Arc, time::Duration};

use futures::Future;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use rumqttc::{
    tokio_rustls::rustls::{ClientConfig, RootCertStore},
    AsyncClient as MqttClient, ClientError, ConnAck, ConnectReturnCode, ConnectionError, Event,
    EventLoop, MqttOptions, Outgoing, Packet, Publish, QoS, Request, TlsConfiguration, Transport,
};
use rustls_native_certs::load_native_certs;
use rustls_pemfile::Item;
use tokio::{
    select, spawn,
    sync::{broadcast, mpsc, watch, Mutex, OnceCell},
    task::JoinSet,
    time::{sleep, Instant},
};
use tracing::{debug_span, error_span, info_span, trace_span, warn_span};
use veil::Redact;

use super::{
    handlers::{
        BridgeDevicesTracker, BridgeExtensionsTracker, BridgeGroupsTracker, BridgeInfoTracker,
        DeviceCapabilitiesManager, DeviceOptionsManager, DeviceRenamer, ExtensionInstaller,
        GroupCreator, GroupDeletor, GroupMemberAdder, GroupMemberRemover, GroupOptionsManager,
        GroupRenamer, HealthChecker, Restarter,
    },
    subscription::TopicSubscription,
};
use crate::{
    background_task,
    error::{Error, ErrorWithMeta},
    event_manager::{EventCore, EventType},
    sync_utils::LockableNotify,
    with_source::ValueWithSource,
    RECONCILE_INTERVAL, RECONCILE_INTERVAL_FAILURE, TIMEOUT,
};

/// Check whether a topic matches a pattern following the MQTT wildcard rules.
fn topic_match(pattern: &str, topic: &str) -> bool {
    let pattern_parts = pattern.split('/').collect::<Vec<&str>>();
    let pattern_length = pattern_parts.len();
    let topic_parts = topic.split('/').collect::<Vec<&str>>();
    for (i, part) in pattern_parts.into_iter().enumerate() {
        match part {
            "#" => {
                if i != pattern_length - 1 {
                    warn_span!("invalid pattern, # is only allowed at the end", pattern);
                }
                return true;
            }
            "+" => continue,
            _ => {
                if part != topic_parts[i] {
                    return false;
                }
            }
        }
    }
    true
}

#[derive(Clone, PartialEq, Debug)]
pub enum ConnectionStatus {
    Pending,
    Active,
    Inactive,
    Closed,
    Failed(String),
    Refused(String),
}
impl From<&ConnectionStatus> for EventCore {
    fn from(val: &ConnectionStatus) -> Self {
        let action = "Connecting".to_string();
        match val {
            ConnectionStatus::Pending => EventCore {
                action,
                note: Some("connecting to broker".into()),
                reason: "Created".into(),
                type_: EventType::Normal,
                ..EventCore::default()
            },
            ConnectionStatus::Active => EventCore {
                action,
                note: Some("successfully connected to broker".into()),
                reason: "Created".into(),
                type_: EventType::Normal,
                ..EventCore::default()
            },
            ConnectionStatus::Inactive => EventCore {
                action,
                note: Some("disconnected from broker".into()),
                reason: "Deleted".into(),
                type_: EventType::Normal,
                ..EventCore::default()
            },
            ConnectionStatus::Closed => EventCore {
                action,
                note: Some("connection closed by broker.".into()),
                reason: "Disconnected".to_string(),
                type_: EventType::Warning,
                ..EventCore::default()
            },
            ConnectionStatus::Failed(err) => EventCore {
                action,
                note: Some(format!("connection failed: {err}")),
                reason: "Created".into(),
                type_: EventType::Warning,
                ..EventCore::default()
            },
            ConnectionStatus::Refused(err) => EventCore {
                action,
                note: Some(format!("connection refused: {err}")),
                reason: "Created".into(),
                type_: EventType::Warning,
                ..EventCore::default()
            },
        }
    }
}
impl TryFrom<&Event> for ConnectionStatus {
    type Error = ();
    fn try_from(value: &Event) -> Result<Self, Self::Error> {
        match value {
            Event::Incoming(Packet::ConnAck(c)) => Ok(c.into()),
            Event::Incoming(Packet::Disconnect) => Ok(ConnectionStatus::Closed),
            Event::Outgoing(Outgoing::Disconnect) => Ok(ConnectionStatus::Inactive),
            _ => Err(()),
        }
    }
}
impl From<&ConnectionError> for ConnectionStatus {
    fn from(value: &ConnectionError) -> Self {
        match value {
            ConnectionError::ConnectionRefused(err) => err.into(),
            err => ConnectionStatus::Failed(err.to_string()),
        }
    }
}
impl From<&ConnAck> for ConnectionStatus {
    fn from(value: &ConnAck) -> Self {
        (&value.code).into()
    }
}
impl From<&ConnectReturnCode> for ConnectionStatus {
    fn from(value: &ConnectReturnCode) -> Self {
        match value {
            ConnectReturnCode::Success => ConnectionStatus::Active,
            ConnectReturnCode::RefusedProtocolVersion => {
                ConnectionStatus::Refused("invalid protocol version".to_string())
            }
            ConnectReturnCode::BadClientId => {
                ConnectionStatus::Refused("invalid client id".to_string())
            }
            ConnectReturnCode::ServiceUnavailable => {
                ConnectionStatus::Refused("service unavailable".to_string())
            }
            ConnectReturnCode::BadUserNamePassword => {
                ConnectionStatus::Refused("bad username/password".to_string())
            }
            ConnectReturnCode::NotAuthorized => {
                ConnectionStatus::Refused("not authorized".to_string())
            }
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Z2MStatus {
    HealthOk,
    HealthError(String),
}
impl From<&Z2MStatus> for EventCore {
    fn from(val: &Z2MStatus) -> Self {
        let action = "Healthcheck".to_string();
        match val {
            Z2MStatus::HealthOk => EventCore {
                action,
                note: Some("zigbee2mqtt healthcheck succeeded".into()),
                reason: "Healthcheck".into(),
                type_: EventType::Normal,
                ..EventCore::default()
            },
            Z2MStatus::HealthError(err) => EventCore {
                action,
                note: Some(format!("zigbee2mqtt healthcheck failed: {err}")),
                reason: "Healthcheck".into(),
                type_: EventType::Warning,
                ..EventCore::default()
            },
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Status {
    ConnectionStatus(ConnectionStatus),
    Z2MStatus(Z2MStatus),
}
impl From<&Status> for EventCore {
    fn from(val: &Status) -> Self {
        match val {
            Status::ConnectionStatus(s) => s.into(),
            Status::Z2MStatus(s) => s.into(),
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub struct Options {
    pub client_id: String,
    pub host: ValueWithSource<String>,
    pub port: ValueWithSource<u16>,
    pub tls: ValueWithSource<Option<OptionsTLS>>,
    pub credentials: ValueWithSource<Option<OptionsCredentials>>,
    pub base_topic: ValueWithSource<String>,
}
#[derive(Eq, PartialEq, Debug, Clone)]
pub struct OptionsTLS {
    pub ca: ValueWithSource<Option<String>>,
    pub client: ValueWithSource<Option<OptionsTLSClient>>,
}
#[derive(Eq, PartialEq, Redact, Clone)]
pub struct OptionsTLSClient {
    pub cert: ValueWithSource<String>,
    #[redact(fixed = 3)]
    pub key: ValueWithSource<String>,
}
#[derive(Eq, PartialEq, Redact, Clone)]
pub struct OptionsCredentials {
    pub username: ValueWithSource<String>,
    #[redact(fixed = 3)]
    pub password: ValueWithSource<String>,
}
impl Options {
    pub(self) fn to_rumqtt(&self) -> Result<MqttOptions, ErrorWithMeta> {
        let mut options =
            MqttOptions::new(self.client_id.clone(), self.host.clone().take(), *self.port);
        if let Some(tls) = self.tls.clone().transpose() {
            let mut store = RootCertStore::empty();
            if let Some(ca) = tls.ca.clone().transpose() {
                let mut ca_bytes = Box::new(ca.as_bytes());
                for cert in rustls_pemfile::read_all(&mut ca_bytes) {
                    match cert {
                        Err(err) => {
                            return Err(Error::InvalidResource(format!(
                                "unable to parse ca certificates: {err}"
                            ))
                            .caused_by(&ca))
                        }
                        Ok(Item::X509Certificate(ref cert)) => {
                            store.add(cert.to_owned()).map_err(|err| {
                                Error::InvalidResource(format!(
                                    "unable to parse ca certificate: {err}"
                                ))
                                .caused_by(&ca)
                            })?;
                        }
                        Ok(item) => {
                            return Err(Error::InvalidResource(format!(
                                "found non-certificate in pem bundle: {item:?}"
                            ))
                            .caused_by(&ca))
                        }
                    }
                }
            } else {
                let result = load_native_certs();
                if result.certs.is_empty() {
                    return Err(Error::InvalidResource(
                        "unable to load default certificates, specify root ca".to_owned(),
                    )
                    .caused_by(&tls.ca));
                }
                let (added, ignored) = store.add_parsable_certificates(result.certs);
                if added == 0 {
                    return Err(Error::InvalidResource(
                        "unable to load any of the system certificates, specify root ca".to_owned(),
                    )
                    .caused_by(&tls.ca));
                }
                if ignored == 0 {
                    info_span!("Loaded all system certificates", added, ignored);
                } else {
                    warn_span!("Loaded some system certificates", added, ignored);
                }
            }

            let config = ClientConfig::builder()
                .with_root_certificates(store)
                .with_no_client_auth();
            options.set_transport(Transport::Tls(TlsConfiguration::Rustls(Arc::new(config))));
        }
        if let Some(cred) = self.credentials.clone().take() {
            options.set_credentials(cred.username.take(), cred.password.take());
        }

        options.set_clean_session(false);
        options.set_keep_alive(Duration::from_secs(15));
        options.set_max_packet_size(50_000_000, 50_000_000);

        Ok(options)
    }
}

async fn client_disconnect_or_warn(client: &MqttClient, id: &String) {
    match client.disconnect().await {
        Ok(())
        | Err(
            ClientError::Request(Request::Disconnect(_))
            | ClientError::TryRequest(Request::Disconnect(_)),
        ) => {}
        Err(err) => {
            warn_span!("failed to disconnect cleanly", id, ?err);
        }
    };
}

struct OnceCellMaybe<T>(OnceCell<Result<Arc<T>, Error>>);
impl<T> OnceCellMaybe<T> {
    fn new() -> Self {
        Self(OnceCell::new())
    }

    pub async fn get_or_init<F, Fut>(&self, f: F) -> Result<Arc<T>, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        self.0
            .get_or_init(|| async { Ok(Arc::new(f().await?)) })
            .await
            .clone()
    }
}

const STATUS_DEBOUNCE: Duration = Duration::from_secs(10);
pub struct Manager {
    id: String,
    options: Mutex<Options>,
    client: Mutex<MqttClient>,
    tasks: Mutex<JoinSet<()>>,
    shutdown_reason: OnceCell<Option<Box<Error>>>,
    shutdown_done: Mutex<watch::Receiver<bool>>,
    event_sender: Mutex<broadcast::Sender<Event>>,
    status_sender: Mutex<mpsc::UnboundedSender<Status>>,
    subscriptions: Mutex<HashMap<String, Arc<Mutex<broadcast::Sender<Publish>>>>>,
    subscription_lock: LockableNotify,
    bridge_info_tracker: OnceCellMaybe<BridgeInfoTracker>,
    bridge_extensions_tracker: OnceCellMaybe<BridgeExtensionsTracker>,
    bridge_devices_tracker: OnceCellMaybe<BridgeDevicesTracker>,
    bridge_groups_tracker: OnceCellMaybe<BridgeGroupsTracker>,
}
impl Manager {
    #[allow(clippy::too_many_lines)]
    pub async fn new(
        options: Options,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Status>), ErrorWithMeta> {
        info_span!("creating Zigbee2MQTT manager", ?options);

        let (status_sender, status_receiver) = mpsc::unbounded_channel();
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);

        let id = format!(
            "{id}#{random}",
            id = options.client_id,
            random = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(8)
                .map(char::from)
                .collect::<String>(),
        );

        macro_rules! wait_for_event {
            ($eventloop:expr, $expected:pat) => {
                loop {
                    let event = $eventloop.poll().await;
                    match event {
                        $expected => break,
                        Err(ref err) => match err.into() {
                            ConnectionStatus::Refused(_) => {},
                            err => {
                                info_span!("unexpected event during initial handshake, ignoring", id, ?err);
                                continue;
                            }
                        },
                        _ => {},
                    };
                    return Err(Error::ActionFailed(
                        format!(
                            "unexpected event during initial handshake, expected {expected}, got {event:?}",
                            expected = stringify!($expected),
                        ),
                        None,
                    ).unknown_cause());
                }
            }
        }

        // We want a persistent session for when we need to reconnect due to connection issues/credential changes, but we don't want state from an earlier MQTTManager.
        let mut mqttoptions = options.to_rumqtt()?;
        let _ = status_sender.send(Status::ConnectionStatus(ConnectionStatus::Pending));

        // To do this we first connect with clean_session=true to clear any existing session for this client id, which should result in session_present=false regardless of whether a session existed.
        debug_span!("clearing old session", ?id);
        mqttoptions.set_clean_session(true);
        let (client, mut eventloop) = MqttClient::new(mqttoptions, 16);
        wait_for_event!(
            eventloop,
            Ok(Event::Incoming(Packet::ConnAck(ConnAck {
                code: ConnectReturnCode::Success,
                session_present: false,
            })))
        );

        // Then we reconnect with clean_session=false to start a new persistent session, which should result in session_present=false as there is no existing session to continue.
        debug_span!("creating new session", ?id);
        eventloop.mqtt_options.set_clean_session(false);
        let _ = client.disconnect().await;
        wait_for_event!(eventloop, Ok(Event::Outgoing(Outgoing::Disconnect)));
        wait_for_event!(
            eventloop,
            Ok(Event::Incoming(Packet::ConnAck(ConnAck {
                code: ConnectReturnCode::Success,
                session_present: false,
            })))
        );

        // Finally we we reconnect _again_ with clean_session=false to continue the new session, which should result in session_present=true. This will be checked in the listener task.
        debug_span!("resuming new session", ?id);
        let _ = client.disconnect().await;
        wait_for_event!(eventloop, Ok(Event::Outgoing(Outgoing::Disconnect)));
        wait_for_event!(
            eventloop,
            Ok(Event::Incoming(Packet::ConnAck(ConnAck {
                code: ConnectReturnCode::Success,
                session_present: true,
            })))
        );
        let _ = status_sender.send(Status::ConnectionStatus(ConnectionStatus::Active));

        let inst = Arc::new(Self {
            id,
            options: Mutex::new(options),
            client: Mutex::new(client),
            tasks: Mutex::new(JoinSet::new()),
            shutdown_reason: OnceCell::new(),
            shutdown_done: Mutex::new(shutdown_receiver),
            event_sender: Mutex::new(broadcast::channel(16).0),
            status_sender: Mutex::new(status_sender),
            subscriptions: Mutex::new(HashMap::new()),
            subscription_lock: LockableNotify::new(),
            bridge_info_tracker: OnceCellMaybe::new(),
            bridge_extensions_tracker: OnceCellMaybe::new(),
            bridge_devices_tracker: OnceCellMaybe::new(),
            bridge_groups_tracker: OnceCellMaybe::new(),
        });

        // Start background tasks.
        let mut tasks = inst.tasks.lock().await;
        macro_rules! spawn_task {
            ($name:expr, $run:expr) => {
                tasks.spawn(background_task!($name, { $run }, {
                    let this = inst.clone();
                    |err| async move {
                        let () = this.start_shutdown(Some(Box::new(err))).await;
                    }
                }));
            };
        }
        spawn_task!("main loop", {
            let this = inst.clone();
            async move {
                this.clone()
                    .task_main(eventloop, shutdown_sender)
                    .await
                    .map_err(|e| e.error().to_owned())
            }
        });
        spawn_task!("Zigbee2MQTT healthcheck", inst.clone().task_healthcheck());
        drop(tasks);

        Ok((inst, status_receiver))
    }

    async fn task_main(
        self: Arc<Self>,
        eventloop: EventLoop,
        shutdown_sender: watch::Sender<bool>,
    ) -> Result<(), ErrorWithMeta> {
        // Until the shutdown procedure is started by setting the shutdown reason we keep running the listener, creating new clients when needed.
        let mut eventloop = eventloop;
        loop {
            self.clone().task_listener(eventloop).await;

            if self.shutdown_reason.initialized() {
                debug_span!("shutdown_reason set, stopping main task", id = self.id);
                break;
            }

            let mut client = self.client.lock().await;
            client_disconnect_or_warn(&client, &self.id).await;
            let (newclient, neweventloop) =
                MqttClient::new(self.options.lock().await.clone().to_rumqtt()?, 10);

            *client = newclient;
            eventloop = neweventloop;
        }

        // Now we can disconnect the client and wait until we see the disconnect event or an error (which will always be Lagged because Closed is impossible as we still hold a reference to the sender, and since it's possible that the disconnect was in self lag period we just call it quits to avoid risking waiting forever at self point).
        let mut receiver = self.event_sender.lock().await.subscribe();
        client_disconnect_or_warn(&*self.client.lock().await, &self.id).await;
        loop {
            select! {
                biased;
                event = receiver.recv() => match event {
                    Ok(Event::Outgoing(Outgoing::Disconnect)) | Err(_) => break,
                    _ => {}
                },
                () = sleep(*TIMEOUT) => {
                    warn_span!("timeout while closing manager", id = self.id);
                    break;
                }
            };
        }

        // Send out a final event indicating that we've stopped.
        let _ = self
            .status_sender
            .lock()
            .await
            .send(Status::ConnectionStatus(ConnectionStatus::Inactive));
        let _ = shutdown_sender.send(true);

        Ok(())
    }

    async fn task_listener(self: Arc<Self>, mut eventloop: EventLoop) {
        debug_span!("starting listener", id = self.id);

        let mut last = (Instant::now(), ConnectionStatus::Pending);
        let _ = self
            .status_sender
            .lock()
            .await
            .send(Status::ConnectionStatus(last.1.clone()));
        loop {
            let status = match eventloop.poll().await {
                Ok(event) => {
                    debug_span!("mqtt event", id = self.id, ?event);
                    let _ = self.event_sender.lock().await.send(event.clone());

                    let status = (&event).try_into().ok();

                    match event {
                        Event::Incoming(Packet::ConnAck(ref ack)) => {
                            if !ack.session_present {
                                let () = self
                                    .start_shutdown(Some(Box::new(Error::Mqtt(
                                        "reconnect failed to continue existing session".to_string(),
                                        None,
                                    ))))
                                    .await;
                                break;
                            }
                        }
                        Event::Outgoing(Outgoing::Disconnect) => {
                            // Something has triggered a disconnect from self side. self means we're either reconnecting or shutting down, either way self listener needs to shut down.
                            debug_span!("outgoing disconnect, closing listener", id = self.id);
                            break;
                        }

                        Event::Incoming(Packet::SubAck(_) | Packet::UnsubAck(_)) => {
                            self.subscription_lock.notify();
                        }

                        Event::Incoming(Packet::Publish(msg)) => {
                            trace_span!(
                                "incoming message",
                                topic = msg.topic,
                                message = ?msg.payload
                            );
                        }

                        _ => {}
                    };

                    status
                }
                Err(err) => {
                    let status: ConnectionStatus = (&err).into();

                    // (Some of) these are retried immediately, potentially leading to many attempts per second, so we throttle the rate they are logged/emitted at to avoid spamming.
                    if status == last.1 && last.0.elapsed() < STATUS_DEBOUNCE {
                        continue;
                    }

                    error_span!("mqtt error", id = self.id, ?err);
                    Some(status)
                }
            };
            if let Some(status) = status {
                last = (Instant::now(), status.clone());
                let _ = self
                    .status_sender
                    .lock()
                    .await
                    .send(Status::ConnectionStatus(status));
            }
        }
    }

    async fn task_healthcheck(self: Arc<Self>) -> Result<(), Error> {
        let mut healthcheck = HealthChecker::new(self.clone()).await?;
        loop {
            let (status, interval) = match healthcheck.get().await {
                Ok(()) => (Z2MStatus::HealthOk, *RECONCILE_INTERVAL),
                Err(err) => (
                    Z2MStatus::HealthError(err.to_string()),
                    *RECONCILE_INTERVAL_FAILURE,
                ),
            };

            let _ = self
                .status_sender
                .lock()
                .await
                .send(Status::Z2MStatus(status));

            sleep(interval).await;
        }
    }

    pub async fn update_options(self: &Arc<Self>, newoptions: Options) -> Result<(), Error> {
        if let Some(err) = self.shutdown_reason.get() {
            return Err(Error::ManagerShutDown(err.clone()));
        }

        let mut options = self.options.lock().await;

        macro_rules! key_if_changed {
            ($($var:ident).+) => {
                if newoptions.$($var).+ != options.$($var).+ {
                    Some(stringify!($($var).+))
                } else {
                    None
                }
            };
        }
        let changes: Vec<&str> = [
            key_if_changed!(client_id),
            key_if_changed!(host),
            key_if_changed!(port),
            key_if_changed!(base_topic),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !changes.is_empty() {
            return Err(Error::ActionFailed(
                format!(
                    "manager must be recreated to apply change to option(s) {vars}",
                    vars = changes.join(", "),
                ),
                None,
            ));
        }

        if *options == newoptions {
            debug_span!("options unchanged", id = self.id);
            return Ok(());
        }

        *options = newoptions;
        info_span!("options changed, reconnecting", id = self.id, ?options);

        client_disconnect_or_warn(&*self.client.lock().await, &self.id).await;

        Ok(())
    }

    async fn start_shutdown(self: &Arc<Self>, error: Option<Box<Error>>) {
        // Set the error. This will cause the main task to start the shutdown procedure once the current listener task finishes. Any further attempts to use the manager will return this error, which will signal to the reconciler that a new instance must be created.
        match self.shutdown_reason.set(error) {
            Ok(()) => {}
            Err(_) => return,
        };

        // Close the current client connection. This will cause the listener task to close.
        let client = self.client.lock().await;
        client_disconnect_or_warn(&client, &self.id).await;
    }

    pub async fn close(self: &Arc<Self>, error: Option<Box<Error>>) -> Error {
        self.start_shutdown(error).await;

        // Once the shutdown has completed kill all remaining tasks.
        let _ = self.shutdown_done.lock().await.wait_for(|done| *done).await;
        self.tasks.lock().await.shutdown().await;

        Error::ManagerShutDown(self.shutdown_reason.get().unwrap().clone())
    }

    /// Subscribe to incoming messages on a topic pattern.
    pub(crate) async fn subscribe_topic(
        self: &Arc<Self>,
        topic: &str,
        queue_size: usize,
    ) -> Result<TopicSubscription, Error> {
        let topic = format!("{}/{}", self.options.lock().await.base_topic, topic);
        let mut subscriptions_l = self.subscriptions.lock().await;
        let subscription = if let Some(sender) = subscriptions_l.get(&topic) {
            sender.lock().await.subscribe()
        } else {
            let (sender, receiver) = broadcast::channel(queue_size);
            let sender = Arc::new(Mutex::new(sender));
            subscriptions_l.insert(topic.to_string(), sender.clone());
            drop(subscriptions_l);

            spawn({
                let this = self.clone();
                let topic = topic.to_string();
                async move {
                    let mut raw_receiver = this.event_sender.lock().await.subscribe();
                    while let Ok(event) = raw_receiver.recv().await {
                        match event {
                            Event::Incoming(Packet::Publish(msg)) => {
                                if topic_match(&topic, &msg.topic) {
                                    match sender.lock().await.send(msg) {
                                        Ok(_) => {}
                                        Err(_) => break, // No more listeners, cleanup transform + subscription.
                                    }
                                }
                            }
                            Event::Incoming(Packet::PingReq) => {
                                // Check if this topic + transform are still needed. This is mostly useful for topics that receive little traffic as these might not attempt a send for a while.
                                if sender.lock().await.receiver_count() == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }

                    let mut subscriptions = this.subscriptions.lock().await;
                    let client = this.client.lock().await;
                    let notify = this.subscription_lock.lock().await;
                    debug_span!("unsubscribing", id = this.id, topic);
                    if let Err(err) = client.unsubscribe(topic.clone()).await {
                        let () = this
                            .start_shutdown(Some(Box::new(Error::Mqtt(
                                format!("unsubscribe from {topic} failed"),
                                Some(Arc::new(err)),
                            ))))
                            .await;
                    }
                    notify.notified().await;
                    subscriptions.remove(&topic);
                }
            });

            receiver
        };

        // Always send a subscription event to the server regardless of whether there already was an active subscription as this will trigger a re-send of retained messages.
        let notify = self.subscription_lock.lock().await;
        debug_span!("subscribing", id = self.id, topic);
        self.client
            .lock()
            .await
            .subscribe(&topic, QoS::AtLeastOnce)
            .await
            .map_err(|err| {
                Error::Mqtt(format!("subscribe to {topic} failed"), Some(Arc::new(err)))
            })?;
        notify.notified().await;

        Ok(TopicSubscription::new(subscription, topic.to_string()))
    }

    /// Publish a message.
    pub(crate) async fn publish<T>(self: &Arc<Self>, topic: &str, message: T) -> Result<(), Error>
    where
        T: Into<Vec<u8>> + Debug,
    {
        let topic = format!("{}/{}", self.options.lock().await.base_topic, topic);
        trace_span!("outgoing message", id = self.id, topic, ?message);
        self.client
            .lock()
            .await
            .publish(&topic, QoS::AtLeastOnce, false, message)
            .await
            .map_err(|err| Error::Mqtt(format!("publish to {topic} failed"), Some(Arc::new(err))))
    }

    pub async fn get_bridge_info_tracker(
        self: &Arc<Self>,
    ) -> Result<Arc<BridgeInfoTracker>, Error> {
        self.bridge_info_tracker
            .get_or_init(|| BridgeInfoTracker::new(self.clone()))
            .await
    }

    pub async fn get_bridge_extensions_tracker(
        self: &Arc<Self>,
    ) -> Result<Arc<BridgeExtensionsTracker>, Error> {
        self.bridge_extensions_tracker
            .get_or_init(|| BridgeExtensionsTracker::new(self.clone()))
            .await
    }

    pub async fn install_extension(
        self: &Arc<Self>,
        name: &'static str,
        code: &'static str,
    ) -> Result<(), ErrorWithMeta> {
        ExtensionInstaller::new(self.clone())
            .await?
            .run(name, code)
            .await
    }

    pub async fn restart_zigbee2mqtt(self: &Arc<Self>) -> Result<(), ErrorWithMeta> {
        Restarter::new(self.clone()).await?.run().await
    }

    pub async fn get_bridge_device_tracker(
        self: &Arc<Self>,
    ) -> Result<Arc<BridgeDevicesTracker>, Error> {
        self.bridge_devices_tracker
            .get_or_init(|| BridgeDevicesTracker::new(self.clone()))
            .await
    }

    pub async fn rename_device(
        self: &Arc<Self>,
        ieee_address: ValueWithSource<String>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        DeviceRenamer::new(self.clone())
            .await?
            .run(ieee_address, friendly_name)
            .await
    }

    pub fn get_device_options_manager(
        self: &Arc<Self>,
        ieee_address: ValueWithSource<String>,
    ) -> DeviceOptionsManager {
        DeviceOptionsManager::new(self.clone(), ieee_address)
    }

    pub async fn get_device_capabilities_manager(
        self: &Arc<Self>,
        ieee_address: ValueWithSource<String>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<DeviceCapabilitiesManager, Error> {
        DeviceCapabilitiesManager::new(self.clone(), ieee_address, friendly_name).await
    }

    pub async fn get_bridge_group_tracker(
        self: &Arc<Self>,
    ) -> Result<Arc<BridgeGroupsTracker>, Error> {
        self.bridge_groups_tracker
            .get_or_init(|| BridgeGroupsTracker::new(self.clone()))
            .await
    }

    pub async fn create_group(
        self: &Arc<Self>,
        id: ValueWithSource<Option<usize>>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<usize, ErrorWithMeta> {
        GroupCreator::new(self.clone())
            .await?
            .run(id, friendly_name)
            .await
    }

    pub async fn delete_group(
        self: &Arc<Self>,
        id: ValueWithSource<usize>,
    ) -> Result<(), ErrorWithMeta> {
        GroupDeletor::new(self.clone()).await?.run(id).await
    }

    pub async fn rename_group(
        self: &Arc<Self>,
        id: ValueWithSource<usize>,
        friendly_name: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        GroupRenamer::new(self.clone())
            .await?
            .run(id, friendly_name)
            .await
    }

    pub async fn add_group_member(
        self: &Arc<Self>,
        group: ValueWithSource<usize>,
        device: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        GroupMemberAdder::new(self.clone())
            .await?
            .run(group, device)
            .await
    }

    pub async fn remove_group_member(
        self: &Arc<Self>,
        group: ValueWithSource<usize>,
        device: ValueWithSource<String>,
    ) -> Result<(), ErrorWithMeta> {
        GroupMemberRemover::new(self.clone())
            .await?
            .run(group, device)
            .await
    }

    pub fn get_group_options_manager(
        self: &Arc<Self>,
        id: ValueWithSource<usize>,
    ) -> GroupOptionsManager {
        GroupOptionsManager::new(self.clone(), id)
    }
}
impl Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("id", &self.id)
            .field("options", &self.options)
            .field("shutdown_reason", &self.shutdown_reason)
            .field("shutdown_done", &self.shutdown_done)
            .field("subscriptions", &self.subscriptions)
            .finish_non_exhaustive()
    }
}
