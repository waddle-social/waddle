//! JavaScript client lifecycle, transport, and event-subscription methods.

use super::{
    build_client_config, disconnect_client, driver_loop, event_dispatch_loop, future_to_promise,
    js_error, mpsc, request_stream_management_ack_command, spawn_local, to_js_value,
    try_request_stream_management_ack_for_pagehide, wasm_bindgen, Function, JsResumeState, JsValue,
    PagehideSmAckEnqueueOutcome, Promise, Rc, RefCell, StoredConfig, WaddleClient,
    WaddleClientInner, WaddleConfig, WaddleResumeState, WasmCommandLane, WasmWebSocket,
};
use crate::lifecycle::{
    admit_connect, begin_retirement, complete_startup, retire_and_wait, startup_allowed,
    track_connection,
};

#[wasm_bindgen]
impl WaddleClient {
    /// Create a disconnected client owning a copy of the supplied configuration.
    /// Register callbacks before calling `connect` to observe session readiness.
    #[wasm_bindgen(constructor)]
    pub fn new(config: WaddleConfig) -> WaddleClient {
        WaddleClient {
            inner: Rc::new(RefCell::new(WaddleClientInner {
                config: StoredConfig::from(&config),
                ownership: crate::lifecycle::BrowserOwnership::default(),
                retirement_promise: None,
                command_lane: None,
                driver_core: None,
                on_message: None,
                on_presence: None,
                on_connected: None,
                on_session_lifecycle: None,
                on_disconnected: None,
                on_error: None,
                on_authentication_failed: None,
                on_message_delivery_acked: None,
                on_message_delivery_failed: None,
                on_message_rejected: None,
                on_mds_displayed: None,
                on_avatar_changed: None,
                on_pubsub_event: None,
                on_call: None,
                on_stream_management: None,
                resume_state: None,
            })),
        }
    }

    /// Return a persistence-ready resume snapshot, or `null` when unavailable or not serializable.
    /// The camelCase object includes counters, resumption eligibility, the server's
    /// optional maximum interval, and timestamped unhandled outbound entries.
    /// Persist snapshots promptly, especially after the synchronous pagehide handoff.
    pub fn resume_state(&self) -> JsValue {
        match self.inner.borrow().resume_state.as_ref() {
            Some(state) => {
                to_js_value(&JsResumeState::from(state.clone())).unwrap_or(JsValue::NULL)
            }
            None => JsValue::NULL,
        }
    }

    /// Clone the current resume snapshot into an opaque handle, or return no handle when
    /// unavailable.
    pub fn resume_state_handle(&self) -> Option<WaddleResumeState> {
        self.inner
            .borrow()
            .resume_state
            .clone()
            .map(|inner| WaddleResumeState { inner })
    }

    /// Replace the callback receiving live messages and inbox pushes as `WaddleMessage` objects.
    pub fn set_on_message(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message = Some(cb);
    }

    /// Replace the callback receiving each parsed `WaddlePresence` object.
    pub fn set_on_presence(&mut self, cb: Function) {
        self.inner.borrow_mut().on_presence = Some(cb);
    }

    /// Replace the zero-argument callback fired when a fresh authenticated session becomes ready.
    pub fn set_on_connected(&mut self, cb: Function) {
        self.inner.borrow_mut().on_connected = Some(cb);
    }

    /// Replace the callback receiving `fresh` for a new session or `resumed` after stream
    /// resumption.
    pub fn set_on_session_lifecycle(&mut self, cb: Function) {
        self.inner.borrow_mut().on_session_lifecycle = Some(cb);
    }

    /// Replace the zero-argument callback fired when the active connection closes.
    pub fn set_on_disconnected(&mut self, cb: Function) {
        self.inner.borrow_mut().on_disconnected = Some(cb);
    }

    /// Replace the callback receiving human-readable connection and stream error descriptions.
    pub fn set_on_error(&mut self, cb: Function) {
        self.inner.borrow_mut().on_error = Some(cb);
    }

    /// Receive a typed SASL authentication rejection as `{ condition }`.
    /// The condition is the protocol token, independent of human diagnostics.
    pub fn set_on_authentication_failed(&mut self, cb: Function) {
        self.inner.borrow_mut().on_authentication_failed = Some(cb);
    }

    /// Replace the callback receiving outbound message IDs acknowledged by stream management.
    pub fn set_on_message_delivery_acked(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message_delivery_acked = Some(cb);
    }

    /// Replace the callback receiving message IDs that stream management could not deliver.
    pub fn set_on_message_delivery_failed(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message_delivery_failed = Some(cb);
    }

    /// Receive a parsed message rejection separately from incoming content.
    /// The callback must correlate its stanza ID and sender with an outbound send.
    pub fn set_on_message_rejected(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message_rejected = Some(cb);
    }

    /// XEP-0490 §3.2 PEP event handler. Invoked once per displayed
    /// item carried in an inbound `urn:xmpp:mds:displayed:0` PEP
    /// event, with a `WaddleMdsDisplayedEntry`-shaped JS value.
    pub fn set_on_mds_displayed(&mut self, cb: Function) {
        self.inner.borrow_mut().on_mds_displayed = Some(cb);
    }

    /// XEP-0084 §4.1 metadata transition handler. Invoked once per peer PEP
    /// event with a `WaddleAvatarChanged`-shaped JS value.
    pub fn set_on_avatar_changed(&mut self, cb: Function) {
        self.inner.borrow_mut().on_avatar_changed = Some(cb);
    }

    /// Generic XEP-0060 pubsub event handler. Invoked once per
    /// inbound `<items/>` event with a `WaddlePubsubEvent`-shaped JS
    /// value.
    pub fn set_on_pubsub_event(&mut self, cb: Function) {
        self.inner.borrow_mut().on_pubsub_event = Some(cb);
    }

    /// Register a callback for inbound XMPP-native call events
    /// (XEP-0353 JMI envelopes + XEP-0166 Jingle session control
    /// carrying a `urn:waddle:transports:livekit:0` transport).
    /// The callback receives a [`crate::WaddleCallEvent`]-shaped object
    /// with a `kind` discriminator and optional `media` / `join` /
    /// `reason` fields. See `messaging::call::parse_call_event` on
    /// the Rust side for the typed input.
    pub fn set_on_call(&mut self, cb: Function) {
        self.inner.borrow_mut().on_call = Some(cb);
    }

    /// Register closed, bounded XEP-0198 lifecycle outcomes. The callback
    /// deliberately receives no stanza XML, stream identifiers, or counters.
    pub fn set_on_stream_management(&mut self, cb: Function) {
        self.inner.borrow_mut().on_stream_management = Some(cb);
    }

    /// Best-effort XEP-0198 acknowledgement request for synchronous browser
    /// pagehide. The Rust runtime produces the typed `<r/>` control element.
    pub fn request_stream_management_ack(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            request_stream_management_ack_command(inner).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Pagehide-only XEP-0198 acknowledgement handoff. This synchronously
    /// drains the admitted FIFO and writes the typed `<r/>` through the
    /// driver's exact socket before returning; it never awaits capacity or a
    /// driver turn. Callers must persist immediately after every outcome.
    pub fn try_request_stream_management_ack_for_pagehide(&self) -> PagehideSmAckEnqueueOutcome {
        try_request_stream_management_ack_for_pagehide(&self.inner)
    }

    /// Open the configured WebSocket and start the connection driver; rejects invalid
    /// configuration or an already active client.
    ///
    /// The Promise resolves after startup. Session readiness is reported by the lifecycle
    /// callbacks.
    pub fn connect(&self) -> Promise {
        let inner = self.inner.clone();
        // Admission is synchronous: a subsequent close call must see even a
        // connect Promise whose Rust future has not been polled yet.
        let admission = admit_connect(&inner);
        future_to_promise(async move {
            let ticket = admission.map_err(js_error)?;
            let result = (|| {
                if !startup_allowed(&inner, ticket) {
                    return Err(js_error(
                        "client startup was retired before opening a transport",
                    ));
                }
                let stored = inner.borrow().config.clone();
                let config = build_client_config(&stored)?;
                let ws = WasmWebSocket::connect(config.transport.endpoint.as_str())
                    .map_err(|err| js_error(format!("failed to open websocket: {:?}", err)))?;
                let retirements = track_connection(&inner, ws.web_socket().clone());
                let (wake_tx, wake_rx) = mpsc::channel(1);
                let command_lane = Rc::new(RefCell::new(WasmCommandLane::new(wake_tx)));
                let (event_tx, event_rx) = mpsc::channel(256);
                inner.borrow_mut().command_lane = Some(command_lane.clone());
                spawn_local(event_dispatch_loop(
                    inner.clone(),
                    event_rx,
                    retirements.events,
                ));
                spawn_local(driver_loop(
                    config,
                    ws,
                    wake_rx,
                    event_tx,
                    inner.clone(),
                    command_lane,
                    retirements.driver,
                ));
                Ok(JsValue::UNDEFINED)
            })();
            complete_startup(&inner, ticket);
            result
        })
    }

    /// Close the active connection through the driver; the Promise resolves after the
    /// disconnect command completes.
    pub fn disconnect(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            disconnect_client(inner).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Prove every owned browser WebSocket is CLOSED and both driver and event
    /// tasks have retired before replacing an account/resource owner.
    ///
    /// The bounded Promise rejects if closure is not yet proven. Ownership stays
    /// fenced and retained for retry; concurrent calls join the same operation.
    pub fn disconnect_and_wait(&self) -> Promise {
        let inner = self.inner.clone();
        if let Some(promise) = inner.borrow().retirement_promise.clone() {
            return promise;
        }
        begin_retirement(&inner);
        let pending_inner = inner.clone();
        let promise = future_to_promise(async move {
            let result = retire_and_wait(pending_inner.clone()).await;
            pending_inner.borrow_mut().retirement_promise = None;
            result.map(|()| JsValue::UNDEFINED)
        });
        inner.borrow_mut().retirement_promise = Some(promise.clone());
        promise
    }
}
