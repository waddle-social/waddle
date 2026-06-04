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

    pub fn send_chat_state(
        &self,
        to: String,
        msg_type: String,
        state: String,
        thread_id: Option<String>,
        thread_parent: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_CHAT_STATES;
            let thread = thread_ref_from_args(thread_id, thread_parent);
            let stanza = build_chat_state_message(&to, &state, &msg_type, thread.as_ref())
                .map_err(|err| js_error(err.to_string()))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_displayed(
        &self,
        to: String,
        msg_type: String,
        message_id: String,
        thread_id: Option<String>,
        thread_parent: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_CHAT_MARKERS;
            let thread = thread_ref_from_args(thread_id, thread_parent);
            let stanza = build_displayed_message(&to, &message_id, &msg_type, thread.as_ref());
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// XEP-0490 §3 publish to `urn:xmpp:mds:displayed:0`. `chat_id` is
    /// the bare JID of the chat (DM contact or MUC room) which becomes
    /// the PEP item id; `stanza_id` is the XEP-0359 id of the latest
    /// displayed message; `stanza_id_by` is the JID that injected
    /// that stanza-id (the MUC room for group chats, the user's own
    /// server for 1:1). The publish carries the spec-mandated
    /// publish-options as preconditions.
    pub fn publish_mds_displayed(
        &self,
        chat_id: String,
        stanza_id: String,
        stanza_id_by: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq_id = uuid::Uuid::new_v4().to_string();
            let chat_id: BareJid = chat_id
                .parse()
                .map_err(|err| js_error(format!("invalid MDS chat id: {err}")))?;
            let stanza_id = StanzaId::new(stanza_id)
                .map_err(|err| js_error(format!("invalid MDS stanza id: {err}")))?;
            let stanza_id_by: BareJid = stanza_id_by
                .parse()
                .map_err(|err| js_error(format!("invalid MDS stanza-id by JID: {err}")))?;
            let iq = build_mds_publish_iq(&iq_id, &chat_id, &stanza_id, &stanza_id_by);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn supports_mds_publish_options(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let iq = build_disco_info_iq(&domain, None);
            let result = send_iq_command(inner, iq).await?;
            let info = parse_disco_info_result(&result, &domain)
                .ok_or_else(|| js_error("invalid disco#info response"))?;
            Ok(JsValue::from_bool(info.features.iter().any(|feature| {
                feature == NS_PUBSUB_PUBLISH_OPTIONS_FEATURE
            })))
        })
    }

    /// XEP-0490 §3.1 catch-up: retrieve every item from the user's
    /// own `urn:xmpp:mds:displayed:0` PEP node. Returns an array of
    /// `WaddleMdsDisplayedEntry` records. An empty array on first
    /// call (no node yet) is normal and not an error.
    pub fn fetch_mds_displayed(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mds_catchup_iq(&iq_id);
            let result = match send_iq_command(inner, iq).await {
                Ok(result) => result,
                // No node yet -> treat as empty catch-up. Surface
                // every other error as a JS rejection.
                Err(_) => return to_js_value::<Vec<WaddleMdsDisplayedEntry>>(&Vec::new()),
            };
            let entries: Vec<WaddleMdsDisplayedEntry> = parse_mds_catchup_result(&result)
                .into_iter()
                .map(mds_entry_to_js)
                .collect();
            to_js_value(&entries)
        })
    }

    /// XEP-0060 explicit subscribe to the MDS node, used as a
    /// fallback path for receiving `+notify` events when the chat
    /// client's presence does not yet carry XEP-0115 caps. The
    /// subscriber JID is derived from the bound session JID (bare).
    pub fn subscribe_mds_displayed(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let bare = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mds_subscribe_iq(&iq_id, &bare);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_reaction(
        &self,
        to: String,
        msg_type: String,
        target_id: String,
        emojis: Vec<String>,
        thread_id: Option<String>,
        thread_parent: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_REACTIONS, NS_HINTS);
            let thread = thread_ref_from_args(thread_id, thread_parent);
            let stanza =
                build_reaction_message(&to, &msg_type, &target_id, &emojis, thread.as_ref());
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

    pub fn send_retraction(
        &self,
        to: String,
        msg_type: String,
        retracts_id: String,
        thread_id: Option<String>,
        thread_parent: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = (NS_RETRACT, NS_HINTS);
            let thread = thread_ref_from_args(thread_id, thread_parent);
            let stanza = build_retraction_message(&to, &msg_type, &retracts_id, thread.as_ref());
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

    pub fn cancel_raw_iq(&self, id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            cancel_iq_command(inner, id).await?;
            Ok(JsValue::UNDEFINED)
        })
    }
}
