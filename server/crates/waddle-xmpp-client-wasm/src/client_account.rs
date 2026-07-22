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

    /// XEP-0357 §5 `<enable/>` IQ. No provider credentials — those
    /// flow through `register_push_device` (XEP-0050) against the
    /// Push Service component.
    pub fn enable_push_notifications(&self, service_jid: String, node: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = waddle_xmpp_client::xep::xep0357::build_xep0357_enable_iq(
                &service_jid,
                &node,
                None,
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// XEP-0357 §6.1 `<disable/>` IQ. A `None`/missing `node` disables
    /// ALL push nodes at the service for this user.
    pub fn disable_push_notifications(&self, service_jid: String, node: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = waddle_xmpp_client::xep::xep0357::build_xep0357_disable_iq(
                &service_jid,
                node.as_deref(),
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// XEP-0050 `register-device` ad-hoc command on `push.<domain>`.
    /// Drives the multi-step dance and resolves to the assigned
    /// XEP-0357 node id. Polymorphic over Web Push / APNs / FCM via
    /// the `platform`-discriminated [`RegisterPushDeviceOptions`].
    ///
    /// Replaces the pre-cutover `ensure_push_node` +
    /// `register_web_push_device` pair: the XEP-0050 result form
    /// carries the assigned node id directly.
    pub fn register_push_device(&self, options: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts: RegisterPushDeviceOptions = serde_wasm_bindgen::from_value(options)
                .map_err(|err| push_registration_js_error("protocol-shape", err.to_string()))?;
            let service_jid = waddle_xmpp_client::push::PushServiceJid::new(&opts.service_jid)
                .map_err(push_registration_client_js_error)?;
            let app_id = waddle_xmpp_client::push::PushAppId::new(&opts.app_id)
                .map_err(push_registration_client_js_error)?;
            let environment = opts.environment;
            let credentials = opts
                .into_credentials()
                .map_err(|err| push_registration_js_error("protocol-shape", err))?;
            let driver = WasmCommandDriver { inner };
            let outcome = waddle_xmpp_client::push::register_push_device(
                &driver,
                &service_jid,
                &app_id,
                environment,
                &credentials,
            )
            .await
            .map_err(push_registration_client_js_error)?;
            to_js_value(&WaddleRegisterDeviceResult {
                node: outcome.node.into_string(),
                device_id: outcome.device_id.into_string(),
            })
        })
    }

    /// Fetch the Push Service's currently-active VAPID public key + kid
    /// via the XEP-0128 disco extension form
    /// (`FORM_TYPE='urn:waddle:push:vapid:0'`). The chat passes
    /// `publicKey` (base64url-no-pad uncompressed SEC1 P-256, 65 bytes
    /// with leading 0x04) to `pushManager.subscribe({ applicationServerKey })`
    /// and tracks `kid` so silent rotations on a server-side key change
    /// re-subscribe transparently.
    ///
    /// Resolves to `null` when the server is reachable but does not
    /// advertise the form (Web Push not configured on this deployment)
    /// so the chat caller can branch into the "foreground-only"
    /// fallback. Rejects on transport / stanza errors and on a
    /// malformed advertisement — the chat MUST NOT degrade silently on
    /// a malformed key (round-trip with the browser would fail anyway,
    /// later and less diagnosable).
    pub fn fetch_vapid_public_key(&self, service_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_disco_info_iq(&service_jid, None);
            let result = send_iq_command(inner, iq).await?;
            verify_iq_from_matches_query(result.attr("from"), &service_jid).map_err(js_error)?;
            let disco = parse_disco_info_result(&result, &service_jid).ok_or_else(|| {
                js_error("Push Service disco#info response missing <query/> payload")
            })?;
            let advertisement = match parse_push_vapid_from_disco_info(&disco) {
                Ok(advertisement) => advertisement,
                Err(PushVapidDiscoParseError::NotAdvertised) => return Ok(JsValue::NULL),
                Err(other) => return Err(js_error(other.to_string())),
            };
            let surfaced = WaddleVapidAdvertisement {
                public_key: advertisement.public_key_base64url,
                kid: advertisement.kid.to_string(),
            };
            to_js_value(&surfaced)
        })
    }

    /// XEP-0050 `disable-device` ad-hoc command on `push.<domain>`.
    /// Single-step `action='execute'` carrying the `node` + `device-id`
    /// fields. The Push Service marks the row inactive — no payload
    /// shape returned, the caller only cares about success vs. error.
    ///
    /// Verifies the response's `from=` matches `service_jid` before
    /// returning (RFC 6120 §8.1.2.1 / §10.5 defense-in-depth — same
    /// pattern as `fetch_vapid_public_key` above).
    pub fn disable_push_device(
        &self,
        service_jid: String,
        node: String,
        device_id: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let form =
                waddle_xmpp_client::push::build_disable_device_submit_form(&node, &device_id);
            let iq = waddle_xmpp_client::xep::xep0050::build_xep0050_command_request(
                &service_jid,
                waddle_xmpp_client::push::DISABLE_DEVICE_NODE,
                waddle_xmpp_client::xep::xep0050::AdHocAction::Execute,
                Some(form),
            );
            let response = send_iq_command(inner, iq).await?;
            verify_iq_from_matches_query(response.attr("from"), &service_jid).map_err(js_error)?;
            Ok(JsValue::UNDEFINED)
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
    ///   clients are preserved verbatim (XEP-0492 §3). The same call
    ///   toggles the Waddle rich XEP-0357 push-summary opt-in
    ///   (`options.richPayloadOptIn`, #719) inside the fallback's
    ///   `<advanced/>`.
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

            let driver = WasmCommandDriver { inner };
            let core_options = waddle_xmpp_client::notification_settings::SetRoomNotificationMode {
                room_jid,
                name: name_override,
                mode: opts.mode,
                rich_payload_opt_in: opts.rich_payload_opt_in,
            };
            let outcome = waddle_xmpp_client::notification_settings::set_room_notification_mode(
                &driver,
                core_options,
            )
            .await
            .map_err(|err| js_error(err.to_string()))?;
            let outcome = match outcome {
                waddle_xmpp_client::notification_settings::SetRoomNotificationModeOutcome::Ok {
                    item,
                } => WaddleSetRoomNotificationModeOutcome::Ok {
                    item: surface_bookmark(item),
                },
                waddle_xmpp_client::notification_settings::SetRoomNotificationModeOutcome::NodeConfigMismatch => {
                    WaddleSetRoomNotificationModeOutcome::NodeConfigMismatch
                }
                waddle_xmpp_client::notification_settings::SetRoomNotificationModeOutcome::Error => {
                    WaddleSetRoomNotificationModeOutcome::Error
                }
            };
            to_js_value(&outcome)
        })
    }

    /// Fetch the user's Waddle DM-bookmark items (issue #720) from PEP,
    /// surfaced as a typed array carrying the XEP-0492 fallback
    /// notification mode + rich-payload opt-in (#719) per direct-chat
    /// contact. The DM counterpart to [`Self::fetch_user_bookmarks`].
    ///
    /// The carrier node `urn:waddle:dm-bookmarks:0` is sparse /
    /// override-only: an item exists ONLY when the DM has an override
    /// beyond the XEP-0492 §3 direct-chat default. A `None` `notify_mode`
    /// means the hosted `<notify/>` carries only identity-scoped
    /// siblings; the chat resolves it against the §3 default (`always`).
    ///
    /// Resolves to an empty array when the PEP node is absent (no DM has
    /// an override yet) — XEP-0163 returns `item-not-found`, caught here
    /// and treated as the empty list rather than rejecting the Promise.
    ///
    /// **Deferred** (same as the MUC path): no XEP-0163 §4.4 `+notify`
    /// self-subscription on the DM node yet, so a change in another
    /// client reaches this one on the next session-ready re-fetch.
    pub fn fetch_dm_bookmarks(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_fetch_dm_bookmarks_iq(&uuid::Uuid::new_v4().to_string());
            let items = match send_iq_command_stanza_aware(inner, iq).await? {
                Ok(elem) => parse_dm_bookmarks_response(&elem),
                Err(stanza_err) if stanza_err.condition == "item-not-found" => Vec::new(),
                Err(stanza_err) => return Err(js_error(stanza_err.to_string())),
            };
            to_js_value(&items)
        })
    }

    /// Set the per-DM XEP-0492 notification mode for one direct-chat
    /// contact by merging into the Waddle DM-bookmark carrier (issue
    /// #720). The DM counterpart to [`Self::set_room_notification_mode`].
    ///
    /// Semantics:
    /// * Parse [`SetDmNotificationModeOptions`]; `dmJid` MUST parse as a
    ///   `BareJid` with a localpart, else a typed JS error (the PEP item
    ///   id is the contact bare JID).
    /// * Fetch existing DM items (first-publish `item-not-found` →
    ///   empty); find the one whose id == `dmJid` and read its hosted
    ///   `<notify/>` via [`read_dm_bookmark_notify`].
    /// * Merge the new mode via [`merge_notify`] (the single §3-conformant
    ///   core shared with the MUC carrier — foreign `<advanced/>` and
    ///   identity-scoped siblings preserved verbatim; rich opt-in (#719)
    ///   toggled).
    /// * The node is sparse / override-only: if the merged `<notify/>`
    ///   collapses to the §3 direct-chat default (`always`, no opt-in, no
    ///   foreign `<advanced/>`) per [`dm_notify_is_default`], RETRACT the
    ///   item and resolve to `Removed` instead of publishing. Otherwise
    ///   publish and resolve to `Ok` with the surfaced item — the chat
    ///   reconciles without a refetch.
    /// * `precondition-not-met` maps to the same `NodeConfigMismatch`
    ///   outcome the room path uses; other stanza errors map to `Error`
    ///   (the condition stays on the Rust side as a `tracing::warn`).
    pub fn set_dm_notification_mode(&self, options: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts: SetDmNotificationModeOptions = serde_wasm_bindgen::from_value(options)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            let dm_jid: jid::BareJid = opts
                .dm_jid
                .parse()
                .map_err(|err: jid::Error| JsValue::from_str(&format!("invalid DM JID: {err}")))?;
            // The DM-bookmark item id is the contact's bare JID — it MUST
            // have a localpart (`<localpart>@<domain>`). A domain-only
            // JID is not a valid direct-chat contact and the PEP service
            // would reject the publish; reject early with a typed error.
            if dm_jid.node().is_none() {
                return Err(JsValue::from_str(
                    "invalid DM JID: DM-bookmark id MUST have a localpart",
                ));
            }

            let driver = WasmCommandDriver { inner };
            let core_options = waddle_xmpp_client::notification_settings::SetDmNotificationMode {
                dm_jid,
                mode: opts.mode,
                rich_payload_opt_in: opts.rich_payload_opt_in,
            };
            let outcome = waddle_xmpp_client::notification_settings::set_dm_notification_mode(
                &driver,
                core_options,
            )
            .await
            .map_err(|err| js_error(err.to_string()))?;
            let outcome = match outcome {
                waddle_xmpp_client::notification_settings::SetDmNotificationModeOutcome::Ok {
                    jid,
                    mode,
                    rich_payload_opt_in,
                } => WaddleSetDmNotificationModeOutcome::Ok {
                    item: surface_dm_bookmark_values(&jid, Some(mode), rich_payload_opt_in),
                },
                waddle_xmpp_client::notification_settings::SetDmNotificationModeOutcome::Removed {
                    jid,
                } => WaddleSetDmNotificationModeOutcome::Removed {
                    jid: jid.to_string(),
                },
                waddle_xmpp_client::notification_settings::SetDmNotificationModeOutcome::NodeConfigMismatch => {
                    WaddleSetDmNotificationModeOutcome::NodeConfigMismatch
                }
                waddle_xmpp_client::notification_settings::SetDmNotificationModeOutcome::Error => {
                    WaddleSetDmNotificationModeOutcome::Error
                }
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

    /// Fetch the user's inbox via XEP-0430 (`urn:xmpp:inbox:1`).
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
            // reused as the embedded MAM `queryid` when responses
            // include forwarded last-message payloads.
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
            let query = fetch_threads_query_from_options(&opts).map_err(js_error)?;
            let iq = build_fetch_threads_iq(&request_id, &query);
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
                        call_thread: e.call_thread.map(|c| WaddleThreadCallAnchor {
                            kind: c.kind.as_token().to_owned(),
                            media: c.media.token_list(),
                        }),
                        call_thread_ended: e.call_thread_ended.map(|c| WaddleThreadCallEnded {
                            ended: c.ended,
                            duration: c.duration,
                        }),
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
            let iq = build_publish_mood_iq(
                &uuid::Uuid::new_v4().to_string(),
                &mood.kind,
                mood.text.as_deref(),
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_mood(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(
                inner,
                build_retract_mood_iq(&uuid::Uuid::new_v4().to_string()),
            )
            .await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn publish_activity(&self, activity_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let activity: WaddleActivityOpts = serde_wasm_bindgen::from_value(activity_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_activity_iq(
                &uuid::Uuid::new_v4().to_string(),
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
            let _ = send_iq_command(
                inner,
                build_retract_activity_iq(&uuid::Uuid::new_v4().to_string()),
            )
            .await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn publish_tune(&self, tune_json: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let tune: WaddleTuneOpts = serde_wasm_bindgen::from_value(tune_json)
                .map_err(|err| js_error(err.to_string()))?;
            let iq = build_publish_tune_iq(
                &uuid::Uuid::new_v4().to_string(),
                &waddle_xmpp_client::pep::UserTune {
                    artist: tune.artist,
                    title: tune.title,
                    source: tune.source,
                    length: tune.length,
                    rating: tune.rating,
                    track: tune.track,
                    uri: tune.uri,
                },
            );
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn retract_tune(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = send_iq_command(
                inner,
                build_retract_tune_iq(&uuid::Uuid::new_v4().to_string()),
            )
            .await?;
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
                    photo_uri: vcard.photo_uri,
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
                photo_uri: payload.photo_uri,
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
                build_pep_items_iq(
                    &uuid::Uuid::new_v4().to_string(),
                    &jid,
                    waddle_xmpp_client::pep::NS_MOOD,
                ),
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
                build_pep_items_iq(
                    &uuid::Uuid::new_v4().to_string(),
                    &jid,
                    waddle_xmpp_client::pep::NS_ACTIVITY,
                ),
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
                build_pep_items_iq(
                    &uuid::Uuid::new_v4().to_string(),
                    &jid,
                    waddle_xmpp_client::pep::NS_TUNE,
                ),
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisterPushDeviceError {
    code: &'static str,
    message: String,
}

fn push_registration_js_error(code: &'static str, message: impl ToString) -> JsValue {
    to_js_value(&RegisterPushDeviceError {
        code,
        message: message.to_string(),
    })
    .unwrap_or_else(|_| JsValue::from_str(&message.to_string()))
}

fn push_registration_client_js_error(error: impl Into<ClientError>) -> JsValue {
    let error = error.into();
    let code = match &error {
        ClientError::PushRegistration(
            waddle_xmpp_client::push::PushRegistrationError::SessionExpired,
        ) => "session-expired",
        ClientError::PushRegistration(_) => "protocol-shape",
        ClientError::StanzaError(_) => "stanza-error",
        ClientError::Disconnected => "disconnected",
        ClientError::TransportClosed | ClientError::RequestCancelled => "disconnected",
        _ => "error",
    };
    push_registration_js_error(code, error.to_string())
}

/// Build the `<extensions xmlns='urn:xmpp:bookmarks:1'>…</extensions>`
/// wrapper around the typed bookmark's foreign children so the merge
/// helper has a single `Element` to walk. Pure / side-effect free.
/// Surface one parsed [`BookmarkItem`] into the JS-facing
/// [`WaddleBookmarkItem`]. Pulls the XEP-0492 fallback `<notify/>`
/// child out of the extensions list and returns it as the typed
/// `notify_mode`. Used by both `fetch_user_bookmarks` (per-item map)
/// and `set_room_notification_mode` (response shaping).
fn surface_bookmark(item: BookmarkItem) -> WaddleBookmarkItem {
    // The sibling-scanning rules (multiple `<notify/>` siblings,
    // fallbackless identity-only elements) live in the shared core
    // helper so the WASM and UniFFI boundaries surface identical
    // state. Round-2 PR #804 review (Codex P2).
    let surfaced = waddle_xmpp_client::xep::xep0492::surface_bookmark_notify(&item.extensions);
    WaddleBookmarkItem {
        jid: item.jid.to_string(),
        name: item.name,
        autojoin: item.autojoin,
        notify_mode: surfaced.fallback_mode,
        rich_payload_opt_in: surfaced.rich_payload_opt_in,
    }
}

/// Parse a DM-bookmark items response into the JS-facing surface
/// (issue #720). Reads each payload's hosted `<notify/>` for the
/// XEP-0492 fallback mode + rich-payload opt-in (#719). A payload
/// missing its `<notify/>` yields `notify_mode = None` (the chat
/// resolves it against the §3 direct-chat default).
fn parse_dm_bookmarks_response(iq: &Element) -> Vec<WaddleDmBookmarkItem> {
    waddle_xmpp_client::notification_settings::parse_dm_bookmark_payloads(iq)
        .into_iter()
        .map(|(jid, payload)| match read_dm_bookmark_notify(&payload) {
            Some(notify) => surface_dm_bookmark(&jid, notify),
            None => WaddleDmBookmarkItem {
                jid: jid.to_string(),
                notify_mode: None,
                rich_payload_opt_in: false,
            },
        })
        .collect()
}

fn fetch_threads_query_from_options(
    opts: &WaddleFetchThreadsOptions,
) -> Result<FetchThreadsQuery<'_>, String> {
    let status = match opts.status {
        Some(WaddleThreadStatusFilter::All) | None => ThreadStatusFilter::All,
        Some(WaddleThreadStatusFilter::Unread) => ThreadStatusFilter::Unread,
        Some(WaddleThreadStatusFilter::Following) => ThreadStatusFilter::Following,
    };
    let sort = match opts.sort {
        Some(WaddleThreadSort::Recent) | None => ThreadSort::Recent,
        Some(WaddleThreadSort::Unread) => ThreadSort::Unread,
        Some(WaddleThreadSort::Replies) => ThreadSort::Replies,
    };
    let active_since = match opts.active_since.as_deref() {
        Some(raw) => Some(
            chrono::DateTime::parse_from_rfc3339(raw)
                .map_err(|err| format!("invalid active_since: {err}"))?
                .with_timezone(&chrono::Utc),
        ),
        None => None,
    };
    let channel = match opts.channel.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(
            raw.parse::<BareJid>()
                .map_err(|err| format!("invalid channel: {err}"))?,
        ),
        _ => None,
    };

    Ok(FetchThreadsQuery {
        page_size: opts.page_size,
        after_cursor: opts.after_cursor.as_deref(),
        status,
        active_since,
        channel,
        search: opts.search.as_deref(),
        sort,
    })
}

/// Surface one DM-bookmark `<notify/>` into the JS-facing
/// [`WaddleDmBookmarkItem`] (issue #720). Used by both
/// `fetch_dm_bookmarks` per-item parsing.
fn surface_dm_bookmark(jid: &jid::BareJid, notify: &Element) -> WaddleDmBookmarkItem {
    let surfaced = waddle_xmpp_client::xep::xep0492::surface_dm_notify(notify);
    surface_dm_bookmark_values(jid, surfaced.fallback_mode, surfaced.rich_payload_opt_in)
}

fn surface_dm_bookmark_values(
    jid: &jid::BareJid,
    notify_mode: Option<waddle_xmpp_client::xep::xep0492::NotifyMode>,
    rich_payload_opt_in: bool,
) -> WaddleDmBookmarkItem {
    WaddleDmBookmarkItem {
        jid: jid.to_string(),
        notify_mode,
        rich_payload_opt_in,
    }
}

/// `CommandDriver` impl that forwards through the WASM client's
/// stanza-aware IQ pipeline. RFC 6120 stanza errors must reach the
/// core notification composer unchanged so it can classify conditions
/// such as `item-not-found` and `precondition-not-met`.
pub(crate) struct WasmCommandDriver {
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
}

impl waddle_xmpp_client::push::CommandDriver for WasmCommandDriver {
    async fn send_iq(
        &self,
        iq: Element,
    ) -> Result<Element, waddle_xmpp_client::error::ClientError> {
        match send_iq_command_stanza_aware(self.inner.clone(), iq)
            .await
            .map_err(js_value_to_client_error)?
        {
            Ok(element) => Ok(element),
            Err(stanza_error) => Err(waddle_xmpp_client::error::ClientError::StanzaError(
                stanza_error,
            )),
        }
    }
}

fn js_value_to_client_error(_js: JsValue) -> waddle_xmpp_client::error::ClientError {
    waddle_xmpp_client::error::ClientError::Disconnected
}

/// Defense-in-depth check for `fetch_vapid_public_key`: refuse to accept
/// a disco#info advertisement whose `from` doesn't match the queried
/// `service_jid`. Without this, a malicious user-server (or a
/// compromised middlebox on the C2S path) could spoof a
/// `<iq from='attacker.tld'>` result carrying its own VAPID key, and
/// the browser would bind its push subscription to attacker-signed
/// JWTs. The XMPP server-side dispatcher already routes responses by
/// stanza id, but the `from` rebinding requires the explicit check at
/// the boundary that consumes the typed disco payload.
///
/// RFC 6120 §8.1.2.1 permits `<iq>` results with an absent `from` ONLY
/// when the reply originates from the user's own server (the
/// bare-domain of the bound JID). The Waddle Push Service is a
/// separate addressable entity at `push.<domain>` (XEP-0357 §1 +
/// XEP-0114 component) — a `from`-stripped reply from such a component
/// is a server-side violation we MUST NOT silently accept, because a
/// compromised C2S could strip `from` instead of spoofing it and
/// bypass this guard. The check here therefore REQUIRES a present
/// `from` that matches `service_jid`.
///
/// JID equality follows RFC 6122 §2.4 / RFC 7622 — domainparts are
/// case-insensitive. Push Service JIDs are always pure domain JIDs
/// (XEP-0114 components), so we don't accept a `service_jid/resource`
/// form: it would be permissively useless for the production input
/// set and would needlessly widen the attack surface. Nameprep is
/// approximated with ASCII-lowercase since every push service JID in
/// waddle is an ASCII subdomain — pulling in `idna` purely for this
/// equality would inflate the WASM bundle.
///
/// Extracted as a pure helper so the Rust test suite can pin the
/// anti-spoof semantics without a full wasm-bindgen test harness.
fn verify_iq_from_matches_query(from_attr: Option<&str>, service_jid: &str) -> Result<(), String> {
    let Some(from) = from_attr else {
        return Err(format!(
            "Push Service disco#info response carries no `from`; RFC 6120 §8.1.2.1 requires components to stamp `from`. Refusing to trust VAPID advertisement (queried JID was {service_jid:?})"
        ));
    };
    if from.eq_ignore_ascii_case(service_jid) {
        return Ok(());
    }
    Err(format!(
        "Push Service disco#info response from {from:?} does not match queried JID {service_jid:?}; refusing to trust VAPID advertisement"
    ))
}

#[cfg(test)]
#[path = "client_account_tests.rs"]
mod tests;
