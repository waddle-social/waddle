//! UniFFI FFI bridge exposing the Waddle XMPP client to Swift.

uniffi::setup_scaffolding!("waddle_xmpp_client");

use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use jid::{BareJid, FullJid};
use minidom::Element;
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use url::Url;

mod boundary_convert;
mod calls;
mod convert;
mod error;
mod jid_parts;
mod messaging;
mod messaging_verbs;
mod notify_settings;
mod push;
mod send_outcome;
mod stanza;
mod types;

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod messaging_verbs_tests;
#[cfg(test)]
mod native_event_pump_tests;
#[cfg(test)]
mod notify_settings_tests;

pub use error::WaddleError;
pub use notify_settings::{
    WaddleBookmarkItem, WaddleDmBookmarkItem, WaddleNotifyMode, WaddleSetDmNotificationModeOutcome,
    WaddleSetRoomNotificationModeOutcome,
};
pub use types::*;

use convert::{event_to_ffi, resume_state_from_ffi};
use waddle_xmpp_client::{
    messaging::SessionId, AccessToken, ClientConfig, ClientEvent, ClientHandle, ConnectionConfig,
    ConnectionEvent, MessageDeliveryEvent, OAuthBearerConfig, StreamManagementEvent,
    WebSocketConfig,
};
use waddle_xmpp_client::{ClientResource, XmppClient};

// ── Main client object ───────────────────────────────────────────────────────

#[derive(uniffi::Object)]
pub struct WaddleClient {
    config: WaddleConfig,
    handle: Mutex<Option<ClientHandle>>,
    event_pump: Mutex<Option<NativeEventPump>>,
    poll_gate: Mutex<()>,
    lifecycle_gate: Mutex<()>,
    diagnostic_tx: mpsc::UnboundedSender<WaddleClientEvent>,
    diagnostic_rx: Mutex<mpsc::UnboundedReceiver<WaddleClientEvent>>,
    lifecycle_epoch: AtomicU64,
    lifecycle_tx: watch::Sender<u64>,
    #[cfg(test)]
    test_diagnostic_sink: Option<Arc<std::sync::Mutex<Vec<String>>>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    #[uniffi::constructor]
    pub fn new(config: WaddleConfig) -> Arc<Self> {
        let (diagnostic_tx, diagnostic_rx) = mpsc::unbounded_channel();
        let (lifecycle_tx, _) = watch::channel(0);
        Arc::new(Self {
            config,
            handle: Mutex::new(None),
            event_pump: Mutex::new(None),
            poll_gate: Mutex::new(()),
            lifecycle_gate: Mutex::new(()),
            diagnostic_tx,
            diagnostic_rx: Mutex::new(diagnostic_rx),
            lifecycle_epoch: AtomicU64::new(0),
            lifecycle_tx,
            #[cfg(test)]
            test_diagnostic_sink: None,
        })
    }

    pub async fn connect(&self) {
        let connect_epoch = {
            let _lifecycle_guard = self.lifecycle_gate.lock().await;
            let epoch = self.lifecycle_epoch.fetch_add(1, Ordering::AcqRel) + 1;
            self.lifecycle_tx.send_replace(epoch);
            epoch
        };
        if let Err(error) =
            uuid::Uuid::parse_str(&self.config.delivery_attempt.attempt_id.value)
        {
            self.emit_error(format!("Invalid delivery attempt UUID: {error}"));
            return;
        }

        let jid: BareJid = match self.config.jid.parse() {
            Ok(j) => j,
            Err(e) => {
                self.emit_error(format!("Invalid JID: {e}"));
                return;
            }
        };

        let domain: BareJid = match jid.domain().to_string().parse() {
            Ok(d) => d,
            Err(e) => {
                self.emit_error(format!("Invalid domain: {e}"));
                return;
            }
        };

        let url: Url = match self.config.server_url.parse() {
            Ok(u) => u,
            Err(e) => {
                self.emit_error(format!("Invalid server URL: {e}"));
                return;
            }
        };

        let transport = match WebSocketConfig::new(url) {
            Ok(t) => t,
            Err(e) => {
                self.emit_error(format!("Invalid WebSocket config: {e}"));
                return;
            }
        };

        let resource = match ClientResource::new(&self.config.resource) {
            Ok(r) => r,
            Err(e) => {
                self.emit_error(format!("Invalid resource: {e}"));
                return;
            }
        };

        let auth =
            OAuthBearerConfig::new(jid, resource, AccessToken::new(&self.config.access_token));
        let auth = match auth {
            Ok(a) => a,
            Err(e) => {
                self.emit_error(format!("Invalid auth config: {e}"));
                return;
            }
        };

        let mut client_config =
            match ClientConfig::new(ConnectionConfig::new(domain), transport, auth) {
                Ok(c) => c,
                Err(e) => {
                    self.emit_error(format!("Invalid client config: {e}"));
                    return;
                }
            };

        // XEP-0198: seed the runtime with the persisted resume snapshot
        // so it attempts <resume/> before resource binding, exactly as
        // the wasm client threads it through StoredConfig.
        if let Some(resume) = self.config.resume_state.clone() {
            match resume_state_from_ffi(resume) {
                Ok(state) => {
                    client_config.session.stream_management.resume_state = Some(state);
                }
                Err(e) => {
                    self.emit_error(format!("Invalid resume state: {e}"));
                    return;
                }
            }
        }

        let xmpp_client = match XmppClient::new(client_config) {
            Ok(c) => c,
            Err(e) => {
                self.emit_error(format!("Failed to create XMPP client: {e}"));
                return;
            }
        };

        let driver = match xmpp_client.driver() {
            Ok(d) => d,
            Err(e) => {
                self.emit_error(format!("Failed to create driver: {e}"));
                return;
            }
        };

        let client_handle = match driver.connect().await {
            Ok(h) => h,
            Err(e) => {
                self.emit_error(format!("Failed to connect: {e}"));
                return;
            }
        };

        let events = client_handle.events();
        let account_bare_jid = self.config.jid.split('/').next().unwrap_or("").to_string();
        let lifecycle_guard = self.lifecycle_gate.lock().await;
        if self.lifecycle_epoch.load(Ordering::Acquire) != connect_epoch {
            drop(lifecycle_guard);
            let _ = client_handle.disconnect().await;
            return;
        }
        *self.event_pump.lock().await = Some(NativeEventPump::new(
            events,
            account_bare_jid,
            self.config.delivery_attempt.clone(),
            self.config.resume_state.is_some(),
            connect_epoch,
        ));
        *self.handle.lock().await = Some(client_handle);
        // A poll may have observed this epoch after connect started but
        // before the pump existed. Publish the same epoch again after
        // installation so that poll can re-check without the poll ever
        // holding `event_pump` across an await.
        self.lifecycle_tx.send_replace(connect_epoch);
        drop(lifecycle_guard);
    }

    /// Pull exactly one ordered native event. The client never reads a
    /// subsequent core event until the app asks again, so a
    /// `ResumeFailed` journal CAS is an actual durability barrier rather
    /// than an advisory callback ordering assumption.
    pub async fn next_event(&self) -> WaddleClientEvent {
        let _poll_guard = self.poll_gate.lock().await;
        let mut lifecycle = self.lifecycle_tx.subscribe();
        loop {
            let expected_epoch = self.lifecycle_epoch.load(Ordering::Acquire);
            let mut pump_guard = self.event_pump.lock().await;
            if let Some(pump) = pump_guard.as_ref() {
                if pump.epoch != expected_epoch {
                    return WaddleClientEvent::Disconnected;
                }
            }

            let mut diagnostics = self.diagnostic_rx.lock().await;
            if let Ok(event) = diagnostics.try_recv() {
                return event;
            }

            let Some(pump) = pump_guard.as_mut() else {
                // `connect` must be able to install the pump while this
                // poll waits. Its post-install notification carries the
                // same epoch; a disconnect or replacement carries a new
                // epoch and fences this poll.
                drop(pump_guard);
                let changed = tokio::select! {
                    biased;
                    changed = lifecycle.changed() => {
                        let _ = changed;
                        true
                    },
                    diagnostic = diagnostics.recv() => {
                        return diagnostic.unwrap_or(WaddleClientEvent::Disconnected);
                    },
                };
                if changed && self.lifecycle_epoch.load(Ordering::Acquire) != expected_epoch {
                    return WaddleClientEvent::Disconnected;
                }
                continue;
            };

            tokio::select! {
                biased;
                changed = lifecycle.changed() => {
                    let _ = changed;
                    if self.lifecycle_epoch.load(Ordering::Acquire) != expected_epoch {
                        return WaddleClientEvent::Disconnected;
                    }
                },
                diagnostic = diagnostics.recv() => {
                    return diagnostic.unwrap_or(WaddleClientEvent::Disconnected);
                },
                event = pump.next_event() => return event,
            }
        }
    }

    pub async fn disconnect(&self) {
        // Wake a foreign pending `next_event` before waiting on either
        // the native handle or pump mutex. Swift task cancellation does
        // not cancel an in-flight UniFFI future.
        let lifecycle_guard = self.lifecycle_gate.lock().await;
        let epoch = self.lifecycle_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.lifecycle_tx.send_replace(epoch);
        let handle = self.handle.lock().await.take();
        *self.event_pump.lock().await = None;
        drop(lifecycle_guard);
        if let Some(h) = handle {
            let _ = h.disconnect().await;
        }
    }
}

/// Ordered translation state held behind [`WaddleClient::next_event`].
/// The core broadcast receiver itself retains all later events while the
/// app commits a returned resume transition.
struct NativeEventPump {
    events: broadcast::Receiver<ClientEvent>,
    account_bare_jid: String,
    attempt: WaddleDeliveryAttemptRef,
    awaiting_resume: bool,
    resume_acked_stanza_ids: BTreeSet<String>,
    resume_failed_stanza_ids: BTreeSet<String>,
    duplicate_resume_failure_count: u64,
    epoch: u64,
    poisoned: bool,
}

impl NativeEventPump {
    fn new(
        events: broadcast::Receiver<ClientEvent>,
        account_bare_jid: String,
        attempt: WaddleDeliveryAttemptRef,
        awaiting_resume: bool,
        epoch: u64,
    ) -> Self {
        Self {
            events,
            account_bare_jid,
            attempt,
            awaiting_resume,
            resume_acked_stanza_ids: BTreeSet::new(),
            resume_failed_stanza_ids: BTreeSet::new(),
            duplicate_resume_failure_count: 0,
            epoch,
            poisoned: false,
        }
    }

    async fn next_event(&mut self) -> WaddleClientEvent {
        if self.poisoned {
            return WaddleClientEvent::Disconnected;
        }
        loop {
            let event = match self.events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Closed) => {
                    self.poisoned = true;
                    return WaddleClientEvent::Disconnected;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    self.poisoned = true;
                    return WaddleClientEvent::Error {
                        description: format!(
                            "native event stream lost {skipped} events; connection self-fenced"
                        ),
                    };
                }
            };

            match event {
                ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                    if self.awaiting_resume =>
                {
                    let stanza_id = stanza_id.to_string();
                    if self.resume_failed_stanza_ids.contains(&stanza_id) {
                        self.poisoned = true;
                        return WaddleClientEvent::Error {
                            description:
                                "resume reported the same stanza acked and failed; connection self-fenced"
                                    .to_string(),
                        };
                    }
                    self.resume_acked_stanza_ids.insert(stanza_id.clone());
                    return WaddleClientEvent::DeliveryAcked {
                        signal: WaddleNativeDeliverySignal {
                            attempt: self.attempt.clone(),
                            stanza_id: WaddleDeliveryStanzaId { value: stanza_id },
                        },
                    };
                }
                ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
                    if self.awaiting_resume =>
                {
                    let stanza_id = stanza_id.to_string();
                    if self.resume_acked_stanza_ids.contains(&stanza_id) {
                        self.poisoned = true;
                        return WaddleClientEvent::Error {
                            description:
                                "resume reported the same stanza acked and failed; connection self-fenced"
                                    .to_string(),
                        };
                    }
                    let inserted = self.resume_failed_stanza_ids.insert(stanza_id);
                    if !inserted {
                        // Duplicate core failure signals for the same old
                        // attempt are idempotent. Keep a saturating metric
                        // without changing the canonical affected set.
                        self.duplicate_resume_failure_count =
                            self.duplicate_resume_failure_count.saturating_add(1);
                    }
                }
                ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::Resumed { .. },
                )) if self.awaiting_resume => {
                    if !self.resume_failed_stanza_ids.is_empty() {
                        self.poisoned = true;
                        return WaddleClientEvent::Error {
                            description:
                                "resume succeeded after failures were buffered; connection self-fenced"
                                    .to_string(),
                        };
                    }
                    self.resume_acked_stanza_ids.clear();
                    self.awaiting_resume = false;
                    return WaddleClientEvent::SessionReady {
                        kind: WaddleSessionReadyKind::Resumed,
                        attempt: self.attempt.clone(),
                    };
                }
                ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::Failed,
                )) if self.awaiting_resume => {
                    let Some(next_generation) = self
                        .attempt
                        .connection_generation
                        .value
                        .checked_add(1)
                    else {
                        self.poisoned = true;
                        return WaddleClientEvent::Error {
                            description:
                                "delivery connection generation exhausted; connection self-fenced"
                                    .to_string(),
                        };
                    };
                    let old = self.attempt.clone();
                    let fresh = WaddleDeliveryAttemptRef {
                        attempt_id: WaddleDeliveryAttemptId {
                            value: uuid::Uuid::new_v4().to_string(),
                        },
                        connection_generation: WaddleConnectionGeneration {
                            value: next_generation,
                        },
                    };
                    let affected = std::mem::take(&mut self.resume_failed_stanza_ids)
                        .into_iter()
                        .map(|value| WaddleDeliveryStanzaId { value })
                        .collect();
                    self.resume_acked_stanza_ids.clear();
                    self.attempt = fresh.clone();
                    self.awaiting_resume = false;
                    return WaddleClientEvent::ResumeFailed {
                        transition: WaddleDeliveryAttemptTransition { old, fresh },
                        affected,
                    };
                }
                ClientEvent::Lifecycle(waddle_xmpp_client::LifecycleEvent::SessionReady(_))
                    if self.awaiting_resume =>
                {
                    self.poisoned = true;
                    return WaddleClientEvent::Error {
                        description:
                            "fresh session became ready before resume transition; connection self-fenced"
                                .to_string(),
                    };
                }
                event => {
                    if let Some(event) =
                        event_to_ffi(event, &self.account_bare_jid, &self.attempt)
                    {
                        return event;
                    }
                }
            }
        }
    }
}

// ── Send helpers ────────────────────────────────────────────────────────────

impl WaddleClient {
    #[cfg(test)]
    fn new_for_test(
        config: WaddleConfig,
        diagnostic_sink: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Arc<Self> {
        let (diagnostic_tx, diagnostic_rx) = mpsc::unbounded_channel();
        let (lifecycle_tx, _) = watch::channel(0);
        Arc::new(Self {
            config,
            handle: Mutex::new(None),
            event_pump: Mutex::new(None),
            poll_gate: Mutex::new(()),
            lifecycle_gate: Mutex::new(()),
            diagnostic_tx,
            diagnostic_rx: Mutex::new(diagnostic_rx),
            lifecycle_epoch: AtomicU64::new(0),
            lifecycle_tx,
            test_diagnostic_sink: Some(diagnostic_sink),
        })
    }

    /// Emit a human-readable diagnostic through the single-event
    /// ordered event stream. Every internal failure path funnels through here so
    /// diagnostics stay out of the typed payload variants.
    pub(crate) fn emit_error(&self, description: String) {
        #[cfg(test)]
        if let Some(sink) = &self.test_diagnostic_sink {
            sink.lock()
                .expect("test diagnostic sink poisoned")
                .push(description.clone());
        }
        let _ = self
            .diagnostic_tx
            .send(WaddleClientEvent::Error { description });
    }

    /// Send a built stanza fire-and-forget. Returns `true` on success.
    /// On failure (not connected, transport closed) emits an
    /// `Error` event and returns `false` so callers can short-circuit.
    ///
    /// Holds the client mutex only long enough to clone the
    /// [`ClientHandle`] (cheap — it is `Arc`-backed) and releases
    /// before awaiting the underlying send. Holding across the
    /// `send_stanza` await would be safe today (it returns once the
    /// stanza hits the command channel) but matches the same shape
    /// used by `send_iq_or_error`, which *must* release because
    /// `send_iq` awaits the correlated server reply.
    async fn send_stanza_or_error(&self, stanza: Element, op: &'static str) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle.send_stanza(stanza).await {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("{op} failed: {e}"));
                false
            }
        }
    }

    /// Send a built IQ and await its correlated result. Returns
    /// `true` on `<iq type='result'>`, `false` on `<iq type='error'>`
    /// or transport failure (with an `Error` event for both).
    ///
    /// Clones the [`ClientHandle`] under the mutex and **drops the
    /// guard before awaiting** the IQ response. `send_iq` blocks on
    /// a `oneshot::Receiver` until the server replies — holding the
    /// mutex across that wait would serialise every concurrent
    /// FFI call against the slowest in-flight IQ.
    async fn send_iq_or_error(&self, stanza: Element, op: &'static str) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        // Bounded like the fetch-style methods: `send_iq` itself never
        // times out, so a server that accepts but never answers would
        // otherwise suspend the caller's `await` forever.
        match crate::messaging_verbs::send_iq_with_timeout(&handle, stanza).await {
            Ok(_) => true,
            Err(e) => {
                self.emit_error(format!("{op} failed: {e}"));
                false
            }
        }
    }

    /// Take a cheap [`ClientHandle`] clone under the mutex, dropping
    /// the guard before returning so callers can await freely.
    async fn clone_handle(&self) -> Option<ClientHandle> {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.emit_error("Not connected".to_string());
                None
            }
            Some(h) => {
                let handle = h.clone();
                drop(guard);
                Some(handle)
            }
        }
    }

    fn parse_full_jid(&self, value: &str, op: &'static str) -> Option<FullJid> {
        match value.parse::<FullJid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.emit_error(format!("{op} failed: invalid full JID '{value}': {e}"));
                None
            }
        }
    }

    fn parse_bare_jid(&self, value: &str, op: &'static str) -> Option<BareJid> {
        match value.parse::<BareJid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.emit_error(format!("{op} failed: invalid bare JID '{value}': {e}"));
                None
            }
        }
    }

    fn parse_jid(&self, value: &str, op: &'static str) -> Option<jid::Jid> {
        match value.parse::<jid::Jid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.emit_error(format!("{op} failed: invalid JID '{value}': {e}"));
                None
            }
        }
    }

    /// Wrap a caller-supplied id in [`SessionId`] after rejecting
    /// empty or whitespace-only inputs. XEP-0166 §7 uses `sid` as
    /// the correlation key for every later stanza in the call; a
    /// whitespace-only string passes the empty check but never
    /// matches anything on the wire, leaving the call UI in a
    /// stuck state. `SessionId(String)` is a thin newtype with no
    /// runtime check, so this method is the gate.
    fn parse_session_id(&self, value: String, op: &'static str) -> Option<SessionId> {
        if value.trim().is_empty() {
            self.emit_error(format!(
                "{op} failed: session id must be a non-empty, non-whitespace string"
            ));
            return None;
        }
        Some(SessionId(value))
    }
}
