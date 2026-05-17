use super::*;

#[wasm_bindgen]
impl WaddleClient {
    #[wasm_bindgen(constructor)]
    pub fn new(config: WaddleConfig) -> WaddleClient {
        WaddleClient {
            inner: Rc::new(RefCell::new(WaddleClientInner {
                config: StoredConfig::from(&config),
                cmd_tx: None,
                on_message: None,
                on_presence: None,
                on_connected: None,
                on_session_lifecycle: None,
                on_disconnected: None,
                on_error: None,
                on_message_delivery_acked: None,
                on_message_delivery_failed: None,
                on_mds_displayed: None,
                resume_state: None,
            })),
        }
    }

    pub fn get_resume_state(&self) -> JsValue {
        match self.inner.borrow().resume_state.as_ref() {
            Some(state) => {
                to_js_value(&JsResumeState::from(state.clone())).unwrap_or(JsValue::NULL)
            }
            None => JsValue::NULL,
        }
    }

    pub fn get_resume_state_handle(&self) -> Option<WaddleResumeState> {
        self.inner
            .borrow()
            .resume_state
            .clone()
            .map(|inner| WaddleResumeState { inner })
    }

    pub fn set_on_message(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message = Some(cb);
    }

    pub fn set_on_presence(&mut self, cb: Function) {
        self.inner.borrow_mut().on_presence = Some(cb);
    }

    pub fn set_on_connected(&mut self, cb: Function) {
        self.inner.borrow_mut().on_connected = Some(cb);
    }

    pub fn set_on_session_lifecycle(&mut self, cb: Function) {
        self.inner.borrow_mut().on_session_lifecycle = Some(cb);
    }

    pub fn set_on_disconnected(&mut self, cb: Function) {
        self.inner.borrow_mut().on_disconnected = Some(cb);
    }

    pub fn set_on_error(&mut self, cb: Function) {
        self.inner.borrow_mut().on_error = Some(cb);
    }

    pub fn set_on_message_delivery_acked(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message_delivery_acked = Some(cb);
    }

    pub fn set_on_message_delivery_failed(&mut self, cb: Function) {
        self.inner.borrow_mut().on_message_delivery_failed = Some(cb);
    }

    /// XEP-0490 §3.2 PEP event handler. Invoked once per displayed
    /// item carried in an inbound `urn:xmpp:mds:displayed:0` PEP
    /// event, with a `WaddleMdsDisplayedEntry`-shaped JS value.
    pub fn set_on_mds_displayed(&mut self, cb: Function) {
        self.inner.borrow_mut().on_mds_displayed = Some(cb);
    }

    pub fn connect(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            if inner.borrow().cmd_tx.is_some() {
                return Err(js_error("client is already connected"));
            }

            let stored = inner.borrow().config.clone();
            let config = build_client_config(&stored)?;
            let ws = WasmWebSocket::connect(config.transport.endpoint.as_str())
                .map_err(|err| js_error(format!("failed to open websocket: {:?}", err)))?;
            let (cmd_tx, cmd_rx) = mpsc::channel(64);
            let (event_tx, event_rx) = mpsc::channel(256);

            inner.borrow_mut().cmd_tx = Some(cmd_tx);

            spawn_local(event_dispatch_loop(inner.clone(), event_rx));
            spawn_local(driver_loop(config, ws, cmd_rx, event_tx));

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
}
