use super::*;

#[wasm_bindgen]
impl WaddleClient {
    pub fn subscribe_to_presence(&self, peer_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr("type", "subscribe")
                .attr("to", peer_jid.as_str())
                .build();
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn enable_push_notifications(
        &self,
        service_jid: String,
        node: String,
        token: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_enable_push_iq(&service_jid, &node, &token);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn disable_push_notifications(&self, service_jid: String, node: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_disable_push_iq(&service_jid, &node);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn get_server_version(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let iq = Element::builder("iq", NS_CLIENT)
                .attr("type", "get")
                .attr("to", domain.as_str())
                .attr("id", uuid::Uuid::new_v4().to_string())
                .append(Element::builder("query", NS_VERSION).build())
                .build();
            let result = match send_iq_command(inner, iq).await {
                Ok(result) => result,
                Err(_) => return Ok(JsValue::NULL),
            };
            let Some(query) = result.get_child("query", NS_VERSION) else {
                return Ok(JsValue::NULL);
            };
            let version = WaddleServerVersion {
                name: query
                    .get_child("name", NS_VERSION)
                    .map(|child| child.text()),
                version: query
                    .get_child("version", NS_VERSION)
                    .map(|child| child.text()),
                os: query.get_child("os", NS_VERSION).map(|child| child.text()),
            };
            to_js_value(&version)
        })
    }

    pub fn list_room_members(&self, room_jid: String, affiliation: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_MUC_ADMIN;
            let affiliation = parse_muc_affiliation(&affiliation)?;
            let iq = build_muc_admin_affiliation_list_iq(&room_jid, affiliation);
            let result = send_iq_command(inner, iq).await?;
            let members = parse_muc_admin_affiliation_query(&result)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|item| {
                    Some(WaddleRoomMember {
                        jid: item.jid?,
                        affiliation: item.affiliation.map(muc_affiliation_to_string)?,
                    })
                })
                .collect::<Vec<_>>();
            to_js_value(&members)
        })
    }

    pub fn set_room_affiliation(
        &self,
        room_jid: String,
        jid: String,
        affiliation: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_MUC_ADMIN;
            let item = MucAdminAffiliationItem {
                jid: Some(jid),
                nick: None,
                affiliation: Some(parse_muc_affiliation(&affiliation)?),
                reason: None,
            };
            let iq = build_muc_admin_affiliation_set_iq(&room_jid, &[item]);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn fetch_inbox(&self, opts: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts = if opts.is_null() || opts.is_undefined() {
                WaddleFetchInboxOptions::default()
            } else {
                serde_wasm_bindgen::from_value(opts).map_err(|err| js_error(err.to_string()))?
            };
            let bare_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq = build_waddle_inbox_query_iq(
                bare_jid.as_str(),
                &WaddleInboxQuery {
                    since: opts.since.and_then(|value| u64::try_from(value).ok()),
                    only_unread: opts.only_unread,
                    room: opts.room,
                    threads: opts.threads,
                },
            );
            let result = send_iq_command(inner, iq).await?;
            let inbox = parse_waddle_inbox_result(&result)
                .map(inbox_result_to_js)
                .unwrap_or(WaddleInboxResult {
                    total_unread: 0,
                    conversations: Vec::new(),
                });
            to_js_value(&inbox)
        })
    }

    /// Fetch the global threads view (`urn:waddle:threads:0`).
    /// Returns a `WaddleThreadsPage` (empty page on transport failure).
    pub fn fetch_threads(&self, opts: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts: WaddleFetchThreadsOptions = if opts.is_null() || opts.is_undefined() {
                WaddleFetchThreadsOptions::default()
            } else {
                serde_wasm_bindgen::from_value(opts).map_err(|err| js_error(err.to_string()))?
            };
            let request_id = uuid::Uuid::new_v4().to_string();
            let iq =
                build_fetch_threads_iq(&request_id, opts.page_size, opts.after_cursor.as_deref());
            let page = match send_iq_command(inner, iq).await {
                Ok(result) => parse_threads_response(&result).unwrap_or_default(),
                Err(_) => Default::default(),
            };
            let payload = WaddleThreadsPage {
                total: page.total,
                unread_threads: page.unread_threads,
                entries: page
                    .entries
                    .into_iter()
                    .map(|e| WaddleThreadEntry {
                        channel: e.channel,
                        thread_id: e.thread_id,
                        last_stanza_id: e.last_stanza_id,
                        last_activity: e.last_activity,
                        unread: e.unread,
                        reply_count: e.reply_count,
                        has_unread: e.has_unread,
                        root_author: e.root_author,
                        preview: e.preview,
                        thread_title: e.thread_title,
                    })
                    .collect(),
                next_cursor: page.next_cursor,
            };
            to_js_value(&payload)
        })
    }

    pub fn mark_inbox_read(&self, partner_jid: String, thread_id: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let bare_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq = build_waddle_inbox_mark_read_iq(
                bare_jid.as_str(),
                &WaddleInboxMarkRead {
                    partner: partner_jid,
                    thread: thread_id,
                },
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn list_roster_contacts(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_ROSTER;
            let result = send_iq_command(inner, build_roster_get_iq(None, None)).await?;
            let contacts = parse_roster_result(&result)
                .map(|roster| {
                    roster
                        .items
                        .into_iter()
                        .map(|item| WaddleRosterContact {
                            jid: item.jid.to_string(),
                            name: item.name,
                            subscription: Some(item.subscription.to_string()),
                            groups: item.groups,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            to_js_value(&contacts)
        })
    }

    pub fn publish_mood(&self, mood_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let mood: WaddleMoodOpts = serde_wasm_bindgen::from_value(mood_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_mood_iq(&mood.kind, mood.text.as_deref());
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_mood(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(inner, build_retract_mood_iq()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn publish_activity(&self, activity_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let activity: WaddleActivityOpts = serde_wasm_bindgen::from_value(activity_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_activity_iq(
                &activity.general,
                activity.specific.as_deref(),
                activity.text.as_deref(),
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_activity(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(inner, build_retract_activity_iq()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn publish_tune(&self, tune_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let tune: WaddleTuneOpts = serde_wasm_bindgen::from_value(tune_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_tune_iq(
                tune.artist.as_deref(),
                tune.title.as_deref(),
                tune.source.as_deref(),
                tune.length,
                tune.rating,
                tune.track.as_deref(),
                tune.uri.as_deref(),
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_tune(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(inner, build_retract_tune_iq()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn fetch_vcard4(&self, jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let request_id = uuid::Uuid::new_v4().to_string();
            let iq = build_fetch_vcard4_iq(&jid, &request_id);
            let vcard = match send_iq_command(inner, iq).await {
                Ok(result) => parse_pep_vcard4(&result),
                Err(_) => None,
            };
            match vcard {
                Some(vcard) => to_js_value(&WaddleVCard4 {
                    full_name: vcard.full_name,
                    nickname: vcard.nickname,
                    pronouns: vcard.pronouns,
                    note: vcard.note,
                    url: vcard.url,
                }),
                None => Ok(JsValue::NULL),
            }
        })
    }

    pub fn publish_vcard4(&self, vcard_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let payload: WaddleVCard4 = serde_wasm_bindgen::from_value(vcard_json)
                .map_err(|err| js_error(err.to_string()))?;
            let vcard = VCard4 {
                full_name: payload.full_name,
                nickname: payload.nickname,
                pronouns: payload.pronouns,
                note: payload.note,
                url: payload.url,
            };
            let request_id = uuid::Uuid::new_v4().to_string();
            let iq = build_publish_vcard4_iq(&vcard, &request_id);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn fetch_user_pep_profile(&self, jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let mood = send_iq_command(
                inner.clone(),
                build_pep_items_iq(&jid, waddle_xmpp_client::pep::NS_MOOD),
            )
            .await
            .ok()
            .and_then(|result| parse_pep_mood(&result))
            .map(|mood| WaddleMoodResult {
                kind: mood.mood,
                text: mood.text,
            });
            let activity = send_iq_command(
                inner.clone(),
                build_pep_items_iq(&jid, waddle_xmpp_client::pep::NS_ACTIVITY),
            )
            .await
            .ok()
            .and_then(|result| parse_pep_activity(&result))
            .map(|activity| WaddleActivityResult {
                general: activity.activity,
                specific: activity.specific,
                text: activity.text,
            });
            let tune = send_iq_command(
                inner,
                build_pep_items_iq(&jid, waddle_xmpp_client::pep::NS_TUNE),
            )
            .await
            .ok()
            .and_then(|result| parse_pep_tune(&result))
            .map(|tune| WaddleTuneResult {
                artist: tune.artist,
                title: tune.title,
                source: tune.source,
                length: tune.length,
                rating: tune.rating,
                track: tune.track,
                uri: tune.uri,
            });
            let profile = WaddlePepProfile {
                mood,
                activity,
                tune,
            };
            to_js_value(&profile)
        })
    }
}
