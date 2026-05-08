use super::*;

#[wasm_bindgen]
impl WaddleClient {
    pub fn join_room(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr("to", to.as_str())
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
                .attr("to", to.as_str())
                .append(
                    Element::builder("x", NS_MUC)
                        .append(
                            Element::builder("history", NS_MUC)
                                .attr("maxstanzas", "0")
                                .build(),
                        )
                        .build(),
                )
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn leave_room(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let to = format!("{room_jid}/{nick}");
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr("to", to.as_str())
                .attr("type", "unavailable")
                .build();
            send_stanza_command(inner, stanza).await?;
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
