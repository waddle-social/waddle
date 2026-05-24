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
            // `environment` is wire-typed via `PushEnvironment` on
            // the options struct — serde rejects unknown variants
            // at the JS↔Rust boundary before reaching the IQ builder.
            let registration = PushDeviceRegistration {
                node: &opts.node,
                device_id: &opts.device_id,
                environment: opts.environment,
                provider_endpoint: non_empty(&opts.provider_endpoint),
                provider_token: non_empty(&opts.provider_token),
                provider_key_material: non_empty(&opts.provider_key_material),
            };
            let iq = build_register_push_device_iq(
                &opts.service_jid,
                PushDevicePlatform::Web,
                &registration,
            );
            let result = send_iq_command(inner, iq).await?;
            let device = result
                .get_child("device", WADDLE_PUSH_SERVICE_NS)
                .ok_or_else(|| JsValue::from_str("register-device response missing <device/>"))?;
            let response = PushServiceDevice {
                id: require_non_empty_attr(device, "id", "register-device")?,
                node: require_non_empty_attr(device, "node", "register-device")?,
                status: PushDeviceStatus::from_wire(&require_non_empty_attr(
                    device,
                    "status",
                    "register-device",
                )?)?,
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
                status: PushDeviceStatus::from_wire(&require_non_empty_attr(
                    device,
                    "status",
                    "disable-device",
                )?)?,
            };
            to_js_value(&response)
        })
    }

    /// Fetch the user's XEP-0402 bookmark items from PEP, surfaced as
    /// a typed array carrying the XEP-0492 fallback notification mode
    /// (when present) for each room. The chat UI uses this to
    /// hydrate per-chat notification controls on connect.
    ///
    /// Resolves to an empty array when the user's PEP `urn:xmpp:bookmarks:1`
    /// node is absent (first publish hasn't happened) or empty —
    /// XEP-0163 PEP returns `item-not-found` in that case, which is
    /// caught here and treated as the empty list rather than
    /// rejecting the Promise. Per XEP-0492 §3, the chat caller
    /// resolves an empty `notify_mode` against the conversation-kind
    /// default.
    ///
    /// **Deferred:** the conformant XEP-0163 §4.4 `+notify` self-
    /// subscription on `urn:xmpp:bookmarks:1` would push every other
    /// client's bookmark publish to this client as a `<message>`
    /// headline. Without it, the chat re-fetches on every fresh
    /// session-ready (see `notifySettingsStore.hydrate` wiring at
    /// `chat/src/shell/chat-app-controller.ts`); a setting changed
    /// in another tab reaches this tab only on the next reconnect.
    /// Wiring the headline route is a meaningful slice of new WASM
    /// plumbing and lands in a separate PR.
    pub fn fetch_user_bookmarks(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_fetch_bookmarks_iq(&uuid::Uuid::new_v4().to_string());
            let items = match send_iq_command_stanza_aware(inner, iq).await? {
                Ok(elem) => parse_bookmarks_response(&elem),
                Err(stanza_err) if stanza_err.condition == "item-not-found" => Vec::new(),
                Err(stanza_err) => return Err(js_error(stanza_err.to_string())),
            };
            let surfaced: Vec<WaddleBookmarkItem> =
                items.into_iter().map(surface_bookmark).collect();
            to_js_value(&surfaced)
        })
    }

    /// Set the per-chat XEP-0492 notification mode for one room by
    /// merging into the user's XEP-0402 bookmark for that room. If no
    /// bookmark exists yet for `room_jid`, one is created with
    /// `autojoin=false` so this call doesn't change join behavior.
    ///
    /// Semantics:
    /// * Fetch existing PEP bookmarks (XEP-0402 §2). A missing PEP
    ///   node (`item-not-found`) is treated as empty rather than a
    ///   hard error — the user's first XEP-0492 publish creates the
    ///   node via XEP-0060 publish-options.
    /// * Find the item whose id matches `room_jid`; if missing,
    ///   construct a fresh item with the given `name` (or `None`).
    /// * Replace the fallback `<notify/>` child via
    ///   [`merge_notify_into_extensions`] — foreign `<advanced/>`
    ///   children and identity-scoped siblings written by other
    ///   clients are preserved verbatim (XEP-0492 §3).
    /// * Publish the merged item back.
    ///
    /// Resolves to the new [`WaddleBookmarkItem`] so the chat UI can
    /// reconcile its store without a follow-up fetch.
    pub fn set_room_notification_mode(&self, options: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts: SetRoomNotificationModeOptions = serde_wasm_bindgen::from_value(options)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            let room_jid: jid::BareJid = opts.room_jid.parse().map_err(|err: jid::Error| {
                JsValue::from_str(&format!("invalid room JID: {err}"))
            })?;
            // XEP-0402 §2.2 bookmark item ids are room bare JIDs —
            // the room MUST have a localpart (`<localpart>@<muc-service>`),
            // a domain-only JID is not a valid bookmark id and the
            // PEP service will reject the publish. Reject early so
            // the caller gets a typed error instead of a stanza
            // error round-trip. Round-13 Copilot review.
            if room_jid.node().is_none() {
                return Err(JsValue::from_str(
                    "invalid room JID: XEP-0402 bookmark id MUST have a localpart",
                ));
            }
            // Empty / whitespace-only `name` would publish
            // `<conference name=""/>`, which is technically valid
            // per the XSD but parser-side `parse_item` treats an
            // empty name as `None` — round-trip asymmetry. Normalize
            // here so the wire shape is consistent. Round-13.
            let name_override = opts
                .name
                .as_deref()
                .map(str::trim)
                .filter(|trimmed| !trimmed.is_empty())
                .map(str::to_string);

            // Fetch existing bookmarks so we can preserve the rest of
            // the bookmark item plus any foreign extension children
            // when we merge in the new <notify/> setting. Treat
            // first-publish item-not-found as empty.
            let fetch_iq = build_fetch_bookmarks_iq(&uuid::Uuid::new_v4().to_string());
            let items = match send_iq_command_stanza_aware(inner.clone(), fetch_iq).await? {
                Ok(elem) => parse_bookmarks_response(&elem),
                Err(stanza_err) if stanza_err.condition == "item-not-found" => Vec::new(),
                Err(stanza_err) => return Err(js_error(stanza_err.to_string())),
            };

            let existing = items.iter().find(|item| item.jid == room_jid);
            let existing_extensions =
                existing.map(|item| build_extensions_wrapper(&item.extensions));
            let merged_extensions =
                merge_notify_into_extensions(existing_extensions.as_ref(), opts.mode);
            let extensions_children: Vec<Element> = merged_extensions.children().cloned().collect();

            let merged_item = BookmarkItem {
                jid: room_jid,
                name: existing
                    .and_then(|item| item.name.clone())
                    .or(name_override),
                autojoin: existing.map(|item| item.autojoin).unwrap_or(false),
                nick: existing.and_then(|item| item.nick.clone()),
                password: existing.and_then(|item| item.password.clone()),
                extensions: extensions_children,
            };

            let publish_iq =
                build_publish_bookmark_iq(&merged_item, &uuid::Uuid::new_v4().to_string());
            // Use the stanza-aware send so the chat layer can
            // distinguish recoverable XEP-0060 conditions (notably
            // `precondition-not-met` on an older XEP-0223-style node
            // configured with `access_model=open`) from transport
            // errors. Round-9 reviewer P2 — we resolve the Promise
            // with a typed JS-object outcome instead of throwing a
            // stringly-typed payload across the boundary. The chat
            // wrapper switches on `outcome.kind` directly.
            let outcome = match send_iq_command_stanza_aware(inner, publish_iq).await? {
                Ok(_) => WaddleSetRoomNotificationModeOutcome::Ok {
                    item: surface_bookmark(merged_item),
                },
                Err(stanza_err) if stanza_err.condition == "precondition-not-met" => {
                    WaddleSetRoomNotificationModeOutcome::NodeConfigMismatch
                }
                Err(stanza_err) => WaddleSetRoomNotificationModeOutcome::Error {
                    condition: stanza_err.condition,
                },
            };
            to_js_value(&outcome)
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

/// Build the `<extensions xmlns='urn:xmpp:bookmarks:1'>…</extensions>`
/// wrapper around the typed bookmark's foreign children so the merge
/// helper has a single `Element` to walk. Pure / side-effect free.
fn build_extensions_wrapper(children: &[Element]) -> Element {
    let mut builder = Element::builder("extensions", waddle_xmpp_client::pep::NS_BOOKMARKS);
    for child in children {
        builder = builder.append(child.clone());
    }
    builder.build()
}

/// Surface one parsed [`BookmarkItem`] into the JS-facing
/// [`WaddleBookmarkItem`]. Pulls the XEP-0492 fallback `<notify/>`
/// child out of the extensions list and returns it as the typed
/// `notify_mode`. Used by both `fetch_user_bookmarks` (per-item map)
/// and `set_room_notification_mode` (response shaping).
fn surface_bookmark(item: BookmarkItem) -> WaddleBookmarkItem {
    let notify_mode = item.extensions.iter().find_map(|ext| {
        if ext.is(
            "notify",
            waddle_xmpp_client::xep::xep0492::NS_NOTIFICATION_SETTINGS,
        ) {
            read_fallback_mode(ext)
        } else {
            None
        }
    });
    WaddleBookmarkItem {
        jid: item.jid.to_string(),
        name: item.name,
        autojoin: item.autojoin,
        notify_mode,
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
