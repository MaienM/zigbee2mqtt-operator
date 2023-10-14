use std::{collections::HashMap, sync::Arc, time::Duration};

use derive_more::{Deref, DerefMut};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use rumqttc::{
    AsyncClient as MqttClient, ClientError, ConnAck, ConnectReturnCode, ConnectionError, Event,
    EventLoop, MqttOptions, Outgoing, Packet, Publish, QoS, Request,
};
use tokio::{
    select, spawn,
    sync::{broadcast, mpsc, watch, Mutex, OnceCell},
    task::JoinHandle,
    time::{sleep, Instant},
};
use tracing::{debug_span, error_span, info_span, warn_span};
use veil::Redact;

use crate::{
    event_manager::{EventCore, EventType},
    Error, TIMEOUT,
};

/// Check whether a topic matches a pattern following the MQTT wildcard rules.
fn topic_match(pattern: &str, topic: &String) -> bool {
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
    return true;
}

#[derive(Clone, PartialEq, Debug)]
pub enum MQTTStatus {
    Connecting,
    Connected,
    Stopped,
    ConnectionClosed,
    ConnectionFailed(String),
    ConnectionRefused(String),
}
impl Into<EventCore> for &MQTTStatus {
    fn into(self) -> EventCore {
        let action = "Connecting".to_string();
        return match self {
            MQTTStatus::Connecting => EventCore {
                action,
                note: Some("connecting to broker".into()),
                reason: "Created".into(),
                type_: EventType::Normal,
                ..EventCore::default()
            },
            MQTTStatus::Connected => EventCore {
                action,
                note: Some("successfully connected to broker".into()),
                reason: "Created".into(),
                type_: EventType::Normal,
                ..EventCore::default()
            },
            MQTTStatus::Stopped => EventCore {
                action,
                note: Some("disconnected from broker".into()),
                reason: "Deleted".into(),
                type_: EventType::Normal,
                ..EventCore::default()
            },
            MQTTStatus::ConnectionClosed => EventCore {
                action,
                note: Some("connection closed by broker.".into()),
                reason: "Disconnected".to_string(),
                type_: EventType::Warning,
                ..EventCore::default()
            },
            MQTTStatus::ConnectionFailed(err) => EventCore {
                action,
                note: Some(format!("connection failed: {err}")),
                reason: "Created".into(),
                type_: EventType::Warning,
                ..EventCore::default()
            },
            MQTTStatus::ConnectionRefused(err) => EventCore {
                action,
                note: Some(format!("connection refused: {err}")),
                reason: "Created".into(),
                type_: EventType::Warning,
                ..EventCore::default()
            },
        };
    }
}

impl TryFrom<&Event> for MQTTStatus {
    type Error = ();

    fn try_from(value: &Event) -> Result<Self, Self::Error> {
        match value {
            Event::Incoming(Packet::ConnAck(c)) => Ok(c.into()),
            Event::Incoming(Packet::Disconnect) => Ok(MQTTStatus::ConnectionClosed),
            Event::Outgoing(Outgoing::Disconnect) => Ok(MQTTStatus::Stopped),
            _ => Err(()),
        }
    }
}
impl From<&ConnectionError> for MQTTStatus {
    fn from(value: &ConnectionError) -> Self {
        match value {
            ConnectionError::ConnectionRefused(err) => err.into(),
            err => MQTTStatus::ConnectionFailed(err.to_string()),
        }
    }
}
impl From<&ConnAck> for MQTTStatus {
    fn from(value: &ConnAck) -> Self {
        match value.code {
            _ => (&value.code).into(),
        }
    }
}
impl From<&ConnectReturnCode> for MQTTStatus {
    fn from(value: &ConnectReturnCode) -> Self {
        match value {
            ConnectReturnCode::Success => MQTTStatus::Connected,
            ConnectReturnCode::RefusedProtocolVersion => {
                MQTTStatus::ConnectionRefused("invalid protocol version".to_string())
            }
            ConnectReturnCode::BadClientId => {
                MQTTStatus::ConnectionRefused("invalid client id".to_string())
            }
            ConnectReturnCode::ServiceUnavailable => {
                MQTTStatus::ConnectionRefused("service unavailable".to_string())
            }
            ConnectReturnCode::BadUserNamePassword => {
                MQTTStatus::ConnectionRefused("bad username/password".to_string())
            }
            ConnectReturnCode::NotAuthorized => {
                MQTTStatus::ConnectionRefused("not authorized".to_string())
            }
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub struct MQTTOptions {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub credentials: Option<MQTTCredentials>,
}
#[derive(Eq, PartialEq, Redact, Clone)]
pub struct MQTTCredentials {
    pub username: String,
    #[redact(fixed = 3)]
    pub password: String,
}
impl MQTTOptions {
    pub(self) fn to_rumqtt(&self) -> MqttOptions {
        let mut options = MqttOptions::new(self.id.clone(), self.host.clone(), self.port);
        if let Some(cred) = &self.credentials {
            options.set_credentials(cred.username.clone(), cred.password.clone());
        }

        options.set_clean_session(false);
        options.set_keep_alive(Duration::from_secs(15));
        options.set_max_packet_size(50_000, 50_000);

        return options;
    }
}

async fn client_disconnect_or_warn(client: &MqttClient, id: &String) {
    match client.disconnect().await {
        Ok(_) => {}
        Err(ClientError::Request(Request::Disconnect(_))) => {}
        Err(ClientError::TryRequest(Request::Disconnect(_))) => {}
        Err(err) => {
            warn_span!("failed to disconnect cleanly", id, ?err);
        }
    };
}

macro_rules! assert_unchanged {
    ($options:ident . $($var:ident).+, $oldvalue:expr) => {
        if $options.$($var).+ != $oldvalue {
            return Err(Error::ActionFailed(
                format!(
                    "manager must be recreated to apply change to option {var}",
                    var = stringify!($($var).+),
                ),
                None,
            ));
        }
    };
}
pub(crate) use assert_unchanged;
#[derive(Deref, DerefMut)]
pub struct TopicSubscription {
    #[deref]
    #[deref_mut]
    receiver: broadcast::Receiver<Publish>,
    topic: String,
}
impl TopicSubscription {
    pub fn new(receiver: broadcast::Receiver<Publish>, topic: String) -> TopicSubscription {
        Self { receiver, topic }
    }

    pub fn get_topic(&self) -> &String {
        &self.topic
    }

    pub fn resubscribe(&self) -> Self {
        Self {
            receiver: self.receiver.resubscribe(),
            topic: self.topic.clone(),
        }
    }
}

const STATUS_DEBOUNCE: Duration = Duration::from_secs(10);
pub struct MQTTManager {
    id: String,
    options: Mutex<MQTTOptions>,
    client: Mutex<MqttClient>,
    shutdown_reason: OnceCell<Option<Box<Error>>>,
    shutdown_done: Mutex<watch::Receiver<bool>>,
    event_sender: Mutex<broadcast::Sender<Event>>,
    status_sender: Mutex<mpsc::UnboundedSender<MQTTStatus>>,
    subscriptions: Mutex<HashMap<String, Arc<Mutex<broadcast::Sender<Publish>>>>>,
}
impl MQTTManager {
    pub async fn new(
        options: MQTTOptions,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<MQTTStatus>), Error> {
        let (status_sender, status_receiver) = mpsc::unbounded_channel();

        let id = format!(
            "{id}#{random}",
            id = options.id,
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
                        Err(_) => continue,
                        _ => {},
                    };
                    return Err(Error::ActionFailed(
                        format!(
                            "unexpected event during initial handshake, expected {expected}, got {event:?}",
                            expected = stringify!($expected),
                        ),
                        None,
                    ));
                }
            }
        }

        // We want a persistent session for when we need to reconnect due to connection issues/credential changes, but we don't want state from an earlier MQTTManager.
        let mut mqttoptions = options.to_rumqtt();
        let _ = status_sender.send(MQTTStatus::Connecting);

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
        let _ = status_sender.send(MQTTStatus::Connected);

        let (shutdown_sender, shutdown_receiver) = watch::channel(false);

        let inst = Arc::new(Self {
            id,
            options: Mutex::new(options),
            client: Mutex::new(client),
            shutdown_reason: OnceCell::new(),
            shutdown_done: Mutex::new(shutdown_receiver),
            event_sender: Mutex::new(broadcast::channel(16).0),
            status_sender: Mutex::new(status_sender),
            subscriptions: Mutex::new(HashMap::new()),
        });

        Arc::new(spawn({
            let this = inst.clone();
            async move {
                // Keep restarting the listener until the shutdown procedure is started by setting the shutdown reason.
                let mut eventloop = eventloop;
                loop {
                    let listener = this.start_listener(eventloop).await;
                    let _ = listener.await;

                    if this.shutdown_reason.initialized() {
                        debug_span!("shutdown_reason set, stopping main task", id = this.id);
                        break;
                    }

                    let mut client = this.client.lock().await;
                    client_disconnect_or_warn(&client, &this.id).await;
                    let (newclient, neweventloop) =
                        MqttClient::new(this.options.lock().await.clone().to_rumqtt(), 10);

                    *client = newclient;
                    eventloop = neweventloop;
                }

                // Now we can disconnect the client and wait until we see the disconnect event (which will close the listener), or an error (which has to be lagged as there is still a reference to the sender, and it's possible that the disconnect was in this lag period, so just call it quits).
                let mut receiver = this.event_sender.lock().await.subscribe();
                client_disconnect_or_warn(&*this.client.lock().await, &this.id).await;
                loop {
                    select! {
                        biased;
                        event = receiver.recv() => match event {
                            Ok(Event::Outgoing(Outgoing::Disconnect)) | Err(_) => break,
                            _ => {}
                        },
                        _ = sleep(TIMEOUT) => {
                            warn_span!("timeout while closing manager", id=this.id);
                            break;
                        }
                    };
                }

                // Send out a final event indicating that we've stopped.
                let _ = this.status_sender.lock().await.send(MQTTStatus::Stopped);
                let _ = shutdown_sender.send(true);
            }
        }));

        return Ok((inst, status_receiver));
    }

    pub async fn update_options(self: &Arc<Self>, newoptions: MQTTOptions) -> Result<(), Error> {
        if let Some(err) = self.shutdown_reason.get() {
            return Err(Error::ManagerShutDown(err.clone()));
        }

        let mut options = self.options.lock().await;
        assert_unchanged!(newoptions.id, options.id);
        assert_unchanged!(newoptions.host, options.host);
        assert_unchanged!(newoptions.port, options.port);

        if *options == newoptions {
            debug_span!("options unchanged", id = self.id);
            return Ok(());
        }
        *options = newoptions;
        info_span!("options changed, reconnecting", id = self.id, ?options);

        client_disconnect_or_warn(&*self.client.lock().await, &self.id).await;

        return Ok(());
    }

    pub async fn close(self: &Arc<Self>, error: Option<Box<Error>>) -> Error {
        // Set the error. This will cause the listener to shut down and the main task to start the shutdown procedure. Any further attempts to use the manager will return this error, which will signal to the reconciler that a new instance must be created.
        let _ = self.shutdown_reason.set(error);
        let _ = self.shutdown_done.lock().await.wait_for(|done| *done).await;
        Error::ManagerShutDown(self.shutdown_reason.get().unwrap().clone())
    }

    async fn start_listener(self: &Arc<Self>, mut eventloop: EventLoop) -> JoinHandle<()> {
        debug_span!("starting listener", id = self.id);
        return spawn({
            let this = self.clone();
            async move {
                let mut last = (Instant::now(), MQTTStatus::Connecting);
                let _ = this.status_sender.lock().await.send(last.1.clone());
                loop {
                    let status = match eventloop.poll().await {
                        Ok(event) => {
                            debug_span!("mqtt event", id = this.id, ?event);
                            let _ = this.event_sender.lock().await.send(event.clone());

                            let status = (&event).try_into().ok();

                            match event {
                                Event::Incoming(Packet::ConnAck(ref ack)) => {
                                    if !ack.session_present {
                                        let _ = this.shutdown_reason.set(Some(Box::new(
                                            Error::MQTTError(
                                                "reconnect failed to continue existing session"
                                                    .to_string(),
                                                None,
                                            ),
                                        )));
                                        break;
                                    }
                                }
                                Event::Outgoing(Outgoing::Disconnect) => {
                                    // Something has triggered a disconnect from this side. This means we're either reconnecting or shutting down, either way this listener needs to shut down.
                                    debug_span!(
                                        "outgoing disconnect, closing listener",
                                        id = this.id
                                    );
                                    break;
                                }

                                _ => {}
                            };

                            status
                        }
                        Err(err) => {
                            let status: MQTTStatus = (&err).into();

                            // (Some of) these are retried immediately, potentially leading to many attempts per second, so we throttle the rate they are logged/emitted at to avoid spamming.
                            if status == last.1 && last.0.elapsed() < STATUS_DEBOUNCE {
                                continue;
                            }

                            error_span!("mqtt error", id = this.id, ?err);
                            Some(status)
                        }
                    };
                    if let Some(status) = status {
                        last = (Instant::now(), status.clone());
                        let _ = this.status_sender.lock().await.send(status);
                    }
                }
            }
        });
    }

    pub fn get_id<'a>(self: &'a Arc<Self>) -> &'a String {
        return &self.id;
    }

    /// Subscribe to incoming messages on a topic pattern.
    pub async fn subscribe_topic(
        self: &Arc<Self>,
        topic: &str,
        queue_size: usize,
    ) -> Result<TopicSubscription, Error> {
        let mut subscriptions_l = self.subscriptions.lock().await;
        let subscription = match subscriptions_l.get(topic) {
            Some(sender) => sender.lock().await.subscribe(),
            None => {
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
                        debug_span!("unsubscribing", id = this.id, topic);
                        if let Err(err) = client.unsubscribe(topic.clone()).await {
                            let _ = this.shutdown_reason.set(Some(Box::new(Error::MQTTError(
                                format!("unsubscribe from {topic} failed"),
                                Some(Arc::new(Box::new(err))),
                            ))));
                        }
                        subscriptions.remove(&topic);
                    }
                });

                receiver
            }
        };

        // Always send a subscription event to the server regardless of whether there already was an active subscription as this will trigger a re-send of retained messages.
        debug_span!("subscribing", id = self.id, topic);
        self.client
            .lock()
            .await
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .map_err(|err| {
                Error::MQTTError(
                    format!("subscribe to {topic} failed"),
                    Some(Arc::new(Box::new(err))),
                )
            })?;

        return Ok(TopicSubscription::new(subscription, topic.to_string()));
    }

    /// Publish a message.
    pub async fn publish<T: Into<Vec<u8>>>(
        self: &Arc<Self>,
        topic: &str,
        message: T,
    ) -> Result<(), Error> {
        debug_span!("publishing", id = self.id, topic);
        self.client
            .lock()
            .await
            .publish(topic, QoS::AtLeastOnce, false, message)
            .await
            .map_err(|err| {
                Error::MQTTError(
                    format!("publish to {topic} failed"),
                    Some(Arc::new(Box::new(err))),
                )
            })
    }
}
