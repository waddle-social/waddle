use super::*;

#[wasm_bindgen]
impl WaddleClient {
    pub fn join_room(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str())
                .append(Element::builder("x", NS_MUC).build())
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn join_room_without_history(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str())
                .append(
                    Element::builder("x", NS_MUC)
                        .append(
                            Element::builder("history", NS_MUC)
                                .attr(minidom::rxml::xml_ncname!("maxstanzas").to_owned(), "0")
                                .build(),
                        )
                        .build(),
                )
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Fetch the current pinned-messages list for a MUC room (#414).
    /// Resolves to a JS array of `WaddlePinEntry`. Empty array if the
    /// room has no pins. Server gates on room occupancy: a non-occupant
    /// caller will get a `<forbidden type='auth'/>` error which surfaces
    /// here as a rejected Promise.
    pub fn fetch_room_pins(&self, room_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_pin_list_iq(&room_jid);
            let result = send_iq_command(inner, iq).await?;
            let entries = parse_pin_list_response(&result);
            let js_entries: Vec<WaddlePinEntry> = entries
                .into_iter()
                .map(crate::conversions::pin_entry_to_js)
                .collect();
            serde_wasm_bindgen::to_value(&js_entries).map_err(|err| js_error(err.to_string()))
        })
    }

    pub fn leave_room(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str())
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "unavailable")
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Update the occupant's MUC Muji presence (XEP-0272).
    ///
    /// - `active=false` (and no preparing): emits a bare `<presence/>`
    ///   without any `<muji/>` child — XEP-0272 §Leaving says the
    ///   absence of the element IS the leave marker.
    /// - `active=true`: emits a Muji presence advertising `<content>`
    ///   children for audio (always) and video (when `video=true`).
    /// - `preparing=true`: emits a `<preparing/>` sentinel per XEP-0272
    ///   §Joining two-phase flow. Typically the client sends this
    ///   first, awaits the room's echo, then re-emits with contents
    ///   declared.
    /// - `hand_raised`/`muted`: append an `<in-call
    ///   xmlns='urn:waddle:in-call:0'>` presence child *alongside*
    ///   `<muji/>` (#1029 raised hand / #1030 mute) carrying one marker
    ///   child per set flag. This is the FFI in-call "set method": the
    ///   caller re-emits its current call presence with the flags
    ///   toggled, and the absence of a marker clears that sub-state for
    ///   everyone (the server drops the stored state). Ignored unless the
    ///   occupant is in the call (`active` or `preparing`), since in-call
    ///   state is meaningless without call participation.
    pub fn update_muji_presence(
        &self,
        room_jid: String,
        nick: String,
        active: bool,
        preparing: bool,
        video: bool,
        hand_raised: bool,
        muted: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let mut builder = Element::builder("presence", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str());
            if preparing || active {
                // CLAUDE.md typed-payloads + XML-hard-rule: build the
                // `<muji/>` element via the canonical helper in
                // waddle-xmpp-client::messaging so the wire shape stays
                // locked to a single definition site rather than
                // re-spelling element/attr names + media tokens at the
                // wasm boundary.
                let muji = waddle_xmpp_client::messaging::build_muji_element(
                    preparing,
                    active, // audio is implied by "active in a call"
                    active && video,
                );
                builder = builder.append(muji);
                if hand_raised || muted {
                    builder = builder.append(
                        waddle_xmpp_client::messaging::build_in_call_presence_state_element(
                            hand_raised,
                            muted,
                        ),
                    );
                }
            }
            builder = builder.append(waddle_xmpp_client::caps::build_client_caps_element());
            send_stanza_command(inner, builder.build()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_presence(&self, status: Option<String>, show: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let mut builder = Element::builder("presence", NS_CLIENT);
            if let Some(status) = status.as_deref() {
                builder =
                    builder.append(Element::builder("status", NS_CLIENT).append(status).build());
            }
            if let Some(show) = show.as_deref() {
                builder = builder.append(Element::builder("show", NS_CLIENT).append(show).build());
            }
            builder = builder.append(waddle_xmpp_client::caps::build_client_caps_element());
            send_stanza_command(inner, builder.build()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn request_avatar(&self, jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let bare: BareJid = jid
                .parse()
                .map_err(|err| js_error(format!("invalid JID: {err}")))?;
            let avatar = request_avatar_with_iq(&bare, |stanza| {
                let inner = inner.clone();
                async move { send_avatar_iq_command(inner, stanza).await }
            })
            .await?;

            match avatar {
                Some(avatar) => to_js_value(&WaddleAvatar {
                    jid: avatar.jid.to_string(),
                    id: avatar.id,
                    mime_type: avatar.mime_type,
                    data: avatar.data,
                    url: avatar.url,
                }),
                None => Ok(JsValue::NULL),
            }
        })
    }

    pub fn discover_upload_service(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let items_iq = build_disco_items_iq(&domain, None);
            let items_result = send_iq_command(inner.clone(), items_iq).await?;
            let items = discovery::parse_disco_items_result(&items_result)
                .ok_or_else(|| js_error("could not parse disco#items result"))?;

            for item in items {
                let info_iq = build_disco_info_iq(&item.jid, None);
                let info_result = send_iq_command(inner.clone(), info_iq).await?;
                if let Some(info) = discovery::parse_disco_info_result(&info_result, &item.jid) {
                    if info.has_feature(discovery::UPLOAD_NS) {
                        return Ok(JsValue::from_str(&item.jid));
                    }
                }
            }

            Ok(JsValue::NULL)
        })
    }

    pub fn request_upload_slot(
        &self,
        service_jid: String,
        filename: String,
        size: u64,
        content_type: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_upload_slot_iq(&service_jid, &filename, size, &content_type);
            let result = send_iq_command(inner, iq).await?;
            let slot = discovery::parse_upload_slot(&result)
                .ok_or_else(|| js_error("could not parse upload slot"))?;
            to_js_value(&upload_slot_to_js(slot))
        })
    }
}
