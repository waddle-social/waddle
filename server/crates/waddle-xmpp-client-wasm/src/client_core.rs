use super::*;

#[wasm_bindgen]
impl WaddleClient {
    #[wasm_bindgen(constructor)]
    pub fn new(config: WaddleConfig) -> WaddleClient {
        let command_owner = Rc::new(RefCell::new(None));
        WaddleClient {
            inner: Rc::new(RefCell::new(WaddleClientInner {
                config: StoredConfig::from(&config),
                command_owner: Rc::downgrade(&command_owner),
                disposed: false,
                on_message: None,
                on_presence: None,
                on_connected: None,
                on_session_lifecycle: None,
                on_stream_management: None,
                on_disconnected: None,
                on_error: None,
                on_message_delivery_acked: None,
                on_message_delivery_failed: None,
                on_mds_displayed: None,
                on_pubsub_event: None,
                on_call: None,
                resume_state: None,
            })),
            _command_owner: command_owner,
        }
    }

    pub fn get_resume_state(&self) -> Result<JsValue, JsValue> {
        match self.inner.borrow().resume_state.as_ref() {
            Some(state) => JsResumeState::try_from(state.clone())
                .map_err(|error| js_error(error.to_string()))
                .and_then(|state| {
                    to_js_value(&state)
                        .map_err(|error| js_error(format!("failed to serialize resume state: {error:?}")))
                }),
            None => Ok(JsValue::NULL),
        }
    }

    pub fn set_on_message(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_message = Some(cb);
        }
    }

    pub fn set_on_presence(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_presence = Some(cb);
        }
    }

    pub fn set_on_connected(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_connected = Some(cb);
        }
    }

    pub fn set_on_session_lifecycle(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_session_lifecycle = Some(cb);
        }
    }

    pub fn set_on_stream_management(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_stream_management = Some(cb);
        }
    }

    pub fn set_on_disconnected(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_disconnected = Some(cb);
        }
    }

    pub fn set_on_error(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_error = Some(cb);
        }
    }

    pub fn set_on_message_delivery_acked(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_message_delivery_acked = Some(cb);
        }
    }

    pub fn set_on_message_delivery_failed(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_message_delivery_failed = Some(cb);
        }
    }

    /// XEP-0490 §3.2 PEP event handler. Invoked once per displayed
    /// item carried in an inbound `urn:xmpp:mds:displayed:0` PEP
    /// event, with a `WaddleMdsDisplayedEntry`-shaped JS value.
    pub fn set_on_mds_displayed(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_mds_displayed = Some(cb);
        }
    }

    /// Generic XEP-0060 pubsub event handler. Invoked once per
    /// inbound `<items/>` event with a `WaddlePubsubEvent`-shaped JS
    /// value.
    pub fn set_on_pubsub_event(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_pubsub_event = Some(cb);
        }
    }

    /// Register a callback for inbound XMPP-native call events
    /// (XEP-0353 JMI envelopes + XEP-0166 Jingle session control
    /// carrying a `urn:waddle:transports:livekit:0` transport).
    /// The callback receives a [`WaddleCallEvent`]-shaped object
    /// with a `kind` discriminator and optional `media` / `join` /
    /// `reason` fields. See `messaging::call::parse_call_event` on
    /// the Rust side for the typed input.
    pub fn set_on_call(&mut self, cb: Function) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.on_call = Some(cb);
        }
    }

    /// Permanently retire this wrapper. Disposal is idempotent and clears the
    /// command sender and every JS callback before any queued driver event can
    /// re-enter application code.
    pub fn dispose(&self) {
        self.inner.borrow_mut().retire();
    }

    pub fn connect(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            if inner.borrow().disposed {
                return Err(js_error("client is disposed"));
            }
            let command_owner = command_owner(&inner)?;
            if command_owner.borrow().is_some() {
                return Err(js_error("client is already connected"));
            }

            let stored = inner.borrow().config.clone();
            let config = build_client_config(&stored)?;
            let ws = WasmWebSocket::connect(config.transport.endpoint.as_str())
                .map_err(|err| js_error(format!("failed to open websocket: {:?}", err)))?;
            let (cmd_tx, cmd_rx) = mpsc::channel(64);
            let (event_tx, event_rx) = mpsc::channel(256);

            *command_owner.borrow_mut() = Some(cmd_tx);

            spawn_local(event_dispatch_loop(inner.clone(), event_rx));
            spawn_local(driver_loop(config, ws, cmd_rx, event_tx, inner.clone()));

            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn disconnect(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            disconnect_client(inner).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Best-effort XEP-0198 acknowledgement request for page lifecycle
    /// handoff. The shared runtime suppresses duplicates while another
    /// request is already awaiting `<a/>`.
    pub fn request_stream_management_ack(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            request_stream_management_ack(inner).await?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

impl Drop for WaddleClient {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.retire();
        } else {
            self._command_owner.borrow_mut().take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> WaddleClient {
        WaddleClient::new(WaddleConfig::new(
            "wss://xmpp.example.test/ws".to_owned(),
            "alice@example.test".to_owned(),
            "token".to_owned(),
            "web".to_owned(),
        ))
    }

    #[test]
    fn dispose_is_idempotent_and_permanently_retires_command_input() {
        let client = test_client();
        let (sender, _receiver) = mpsc::channel(1);
        *client._command_owner.borrow_mut() = Some(sender);

        client.dispose();
        client.dispose();

        let inner = client.inner.borrow();
        assert!(inner.disposed);
        assert!(client._command_owner.borrow().is_none());
        assert!(inner.on_message.is_none());
        assert!(inner.on_presence.is_none());
        assert!(inner.on_connected.is_none());
        assert!(inner.on_session_lifecycle.is_none());
        assert!(inner.on_stream_management.is_none());
        assert!(inner.on_disconnected.is_none());
        assert!(inner.on_error.is_none());
        assert!(inner.on_message_delivery_acked.is_none());
        assert!(inner.on_message_delivery_failed.is_none());
        assert!(inner.on_mds_displayed.is_none());
        assert!(inner.on_pubsub_event.is_none());
        assert!(inner.on_call.is_none());
    }
}
