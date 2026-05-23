use super::*;

#[wasm_bindgen]
impl WaddleClient {
    pub fn subscribe_to_presence(&self, peer_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = Element::builder("presence", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "subscribe")
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    peer_jid.as_str(),
                )
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

    /// `<ensure-node app-id='…'/>` on the Push Service. Resolves to the
    /// stable per-(user, app-id) node id the chat should hand to
    /// `register_web_push_device` and to `enable_push_notifications`.
    /// Idempotent — repeated calls return the same node.
    pub fn ensure_push_node(&self, service_jid: String, app_id: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_ensure_push_node_iq(&service_jid, &app_id);
            let result = send_iq_command(inner, iq).await?;
            let node = result
                .get_child("node", WADDLE_PUSH_SERVICE_NS)
                .ok_or_else(|| JsValue::from_str("ensure-node response missing <node/>"))?;
            // Hard-fail on missing/empty required attrs. The server
            // always emits all three on success; an empty here means
            // the wire response is malformed. Persisting `node.id = ""`
            // would silently brick the user's push pipeline (the
            // subsequent register-device and enable IQs would target
            // an empty node). Surface as a rejected Promise so the
            // chat's `console.warn` chain has something to grep.
            let id = require_non_empty_attr(node, "id", "ensure-node")?;
            let jid = require_non_empty_attr(node, "jid", "ensure-node")?;
            let app_id = require_non_empty_attr(node, "app-id", "ensure-node")?;
            let response = PushServiceNode { id, jid, app_id };
            to_js_value(&response)
        })
    }

    /// `<register-device …><provider-…/></register-device>` on the
    /// Push Service. Idempotent on `(node, device_id)`; subsequent
    /// calls UPDATE the row with the latest Web Push credentials.
    /// All three `provider_*` arguments are required for Web Push
    /// (they map to `PushSubscription.endpoint`, `.keys.auth`,
    /// `.keys.p256dh` respectively). Passing an empty string for any
    /// of them omits the corresponding child from the wire IQ.
    pub fn register_web_push_device(&self, options: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts: RegisterWebPushDeviceOptions = serde_wasm_bindgen::from_value(options)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            let registration = PushDeviceRegistration {
                node: &opts.node,
                device_id: &opts.device_id,
                environment: &opts.environment,
                provider_endpoint: non_empty(&opts.provider_endpoint),
                provider_token: non_empty(&opts.provider_token),
                provider_key_material: non_empty(&opts.provider_key_material),
            };
            let iq = build_register_push_device_iq(&opts.service_jid, "web", &registration);
            let result = send_iq_command(inner, iq).await?;
            let device = result
                .get_child("device", WADDLE_PUSH_SERVICE_NS)
                .ok_or_else(|| JsValue::from_str("register-device response missing <device/>"))?;
            let response = PushServiceDevice {
                id: require_non_empty_attr(device, "id", "register-device")?,
                node: require_non_empty_attr(device, "node", "register-device")?,
                status: require_non_empty_attr(device, "status", "register-device")?,
            };
            to_js_value(&response)
        })
    }

    /// `<disable-device node='…' device-id='…'/>` on the Push Service.
    /// Idempotent — operates on the (node, device-id) row only.
    /// Resolves to the typed `PushServiceDevice` shape so the chat
    /// caller can observe the post-disable status (round-5 Copilot
    /// review on PR #760 — match `register_web_push_device`'s
    /// contract instead of resolving void).
    pub fn disable_push_device(
        &self,
        service_jid: String,
        node: String,
        device_id: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_disable_push_device_iq(&service_jid, &node, &device_id);
            let result = send_iq_command(inner, iq).await?;
            let device = result
                .get_child("device", WADDLE_PUSH_SERVICE_NS)
                .ok_or_else(|| JsValue::from_str("disable-device response missing <device/>"))?;
            let response = PushServiceDevice {
                id: require_non_empty_attr(device, "id", "disable-device")?,
                node: require_non_empty_attr(device, "node", "disable-device")?,
                status: require_non_empty_attr(device, "status", "disable-device")?,
            };
            to_js_value(&response)
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
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), domain.as_str())
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    uuid::Uuid::new_v4().to_string(),
                )
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

    /// Fetch the user's inbox via XEP-0430 (`urn:xmpp:inbox:0`).
    ///
    /// Wire-shape: IQ-get with `<inbox/>`, server streams
    /// `<message><entry/></message>` per conversation, terminating
    /// with `<iq type='result'><fin/></iq>`. The streaming reducer
    /// lives in the wasm driver; this method registers the pending
    /// inbox query, drives the IQ send, and resolves the JS promise
    /// once the closing fin arrives.
    pub fn fetch_inbox(&self, opts: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts = if opts.is_null() || opts.is_undefined() {
                WaddleFetchInboxOptions::default()
            } else {
                serde_wasm_bindgen::from_value(opts).map_err(|err| js_error(err.to_string()))?
            };
            let request_id = uuid::Uuid::new_v4().to_string();
            // XEP-0430: the `<inbox/>` IQ is addressed to the user's
            // bare JID (the inbox is per-user state). The wire-id is
            // also the `queryid` correlation for streamed entries.
            let iq = waddle_xmpp_client::inbox::build_inbox_query_iq_element(
                request_id.as_str(),
                opts.only_unread,
                !opts.no_messages,
            );
            let page = match send_inbox_query_command(inner, iq, request_id).await {
                Ok(page) => page,
                Err(_) => crate::state::InboxPage {
                    entries: Vec::new(),
                    fin: waddle_xmpp_client::inbox::InboxFin::default(),
                },
            };
            let result = inbox_page_to_js(page);
            to_js_value(&result)
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

/// Extract a required non-empty XML attribute from a typed response
/// element. Round-5 review on PR #760 flagged that `unwrap_or_default()`
/// on Push Service response attributes would silently persist empty
/// strings (e.g. `node.id = ""`) and brick subsequent IQs that target
/// the broken value. Hard-fail instead — the server always emits
/// these attrs on success, so an empty here means the wire shape has
/// drifted and the chat must surface the error rather than continue.
fn require_non_empty_attr(
    element: &Element,
    attr: &str,
    response_kind: &str,
) -> Result<String, JsValue> {
    element
        .attr(attr)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            JsValue::from_str(&format!(
                "{response_kind} response missing required attribute '{attr}'"
            ))
        })
}
