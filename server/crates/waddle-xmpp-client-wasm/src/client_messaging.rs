use super::*;

#[wasm_bindgen]
impl WaddleClient {
    pub fn send_chat_message(&self, peer_jid: String, body: String, options: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts = send_options_from_js(options)?;
            let (stanza_id, stanza) = build_outbound_message(&peer_jid, "chat", &body, &opts)
                .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::from_str(stanza_id.as_str()))
        })
    }

    pub fn send_groupchat_message(
        &self,
        room_jid: String,
        body: String,
        options: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts = send_options_from_js(options)?;
            let (stanza_id, stanza) = build_outbound_message(&room_jid, "groupchat", &body, &opts)
                .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::from_str(stanza_id.as_str()))
        })
    }

    pub fn send_chat_state(&self, to: String, msg_type: String, state: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_CHAT_STATES;
            let stanza = build_chat_state_message(&to, &state, &msg_type)
                .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_displayed(&self, to: String, msg_type: String, message_id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_CHAT_MARKERS;
            let stanza = build_displayed_message(&to, &message_id, &msg_type);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_reaction(
        &self,
        to: String,
        msg_type: String,
        target_id: String,
        emojis: Vec<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_REACTIONS, NS_HINTS);
            let stanza = build_reaction_message(&to, &msg_type, &target_id, &emojis);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Publish a pin request (#414). `room_jid` is the bare MUC JID;
    /// `target_stanza_id` is the XEP-0359 `by=room` stanza-id of the
    /// message to pin. Server gates on Owner/Admin affiliation; a
    /// non-admin sender will receive a `<forbidden type='auth'/>`
    /// reply via the inbound message stream.
    pub fn pin_message(&self, room_jid: String, target_stanza_id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = build_pinned_message(&room_jid, &target_stanza_id);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Publish an unpin request (#414). Same authorization rules as
    /// [`Self::pin_message`].
    pub fn unpin_message(&self, room_jid: String, target_stanza_id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = build_unpinned_message(&room_jid, &target_stanza_id);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_retraction(&self, to: String, msg_type: String, retracts_id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_RETRACT, NS_HINTS);
            let stanza = build_retraction_message(&to, &msg_type, &retracts_id);
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_moderation(
        &self,
        to: String,
        msg_type: String,
        target_id: String,
        reason: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_MODERATE, NS_RETRACT);
            let stanza = build_moderation_message(&to, &msg_type, &target_id, reason.as_deref());
            let _ = send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_correction(
        &self,
        to: String,
        msg_type: String,
        body: String,
        replaces_id: String,
        options: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_REPLACE;
            let opts = send_options_from_js(options)?;
            let (message_id, stanza) =
                build_correction_message(&to, &msg_type, &body, &replaces_id, &opts)
                    .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::from_str(message_id.as_str()))
        })
    }

    pub fn send_raw_iq(&self, xml: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = parse_raw_iq(&xml)?;
            let result = send_iq_command(inner, stanza).await?;
            Ok(JsValue::from_str(&element_to_xml_string(&result)?))
        })
    }
}
