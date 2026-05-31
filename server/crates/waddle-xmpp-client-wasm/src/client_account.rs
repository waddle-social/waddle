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
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            let service_jid = opts.service_jid.clone();
            let app_id = opts.app_id.clone();
            let environment = opts.environment;
            let credentials = opts.into_credentials().map_err(js_error)?;
            let driver = WasmCommandDriver { inner };
            let outcome = waddle_xmpp_client::push::register_push_device(
                &driver,
                &service_jid,
                &app_id,
                environment,
                &credentials,
            )
            .await
            .map_err(|err| JsValue::from_str(&err.to_string()))?;
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
            let merged_extensions = merge_notify_into_extensions(
                existing_extensions.as_ref(),
                opts.mode,
                opts.rich_payload_opt_in,
            );
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
                Err(stanza_err) => {
                    // Keep the specific RFC 6120 §8.3 condition on
                    // the Rust side as a diagnostic log; do not leak
                    // it across the JS boundary as a stringly-typed
                    // payload. Round-13 PR compliance — typed
                    // payloads beyond the I/O boundary.
                    tracing::warn!(
                        condition = %stanza_err.condition,
                        "bookmark publish rejected with stanza error",
                    );
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

            // Fetch existing DM items so we merge into the contact's
            // current `<notify/>` (preserving foreign `<advanced/>` +
            // identity-scoped siblings). First-publish item-not-found is
            // empty, not an error.
            let fetch_iq = build_fetch_dm_bookmarks_iq(&uuid::Uuid::new_v4().to_string());
            let items = match send_iq_command_stanza_aware(inner.clone(), fetch_iq).await? {
                Ok(elem) => parse_dm_bookmark_payloads(&elem),
                Err(stanza_err) if stanza_err.condition == "item-not-found" => Vec::new(),
                Err(stanza_err) => return Err(js_error(stanza_err.to_string())),
            };

            let existing_notify = items
                .iter()
                .find(|(jid, _)| *jid == dm_jid)
                .and_then(|(_, payload)| read_dm_bookmark_notify(payload))
                .cloned();
            let merged = merge_notify(
                existing_notify.as_ref(),
                opts.mode,
                opts.rich_payload_opt_in,
            );

            // Sparse / override-only: a DM returned to the XEP-0492 §3
            // direct-chat default (`always`, no opt-in, no foreign
            // `<advanced/>`) has no item — retract it. Absence == default.
            if dm_notify_is_default(&merged, NotifyMode::Always) {
                let retract_iq =
                    build_retract_dm_bookmark_iq(&dm_jid, &uuid::Uuid::new_v4().to_string());
                let outcome = match send_iq_command_stanza_aware(inner, retract_iq).await? {
                    Ok(_) => WaddleSetDmNotificationModeOutcome::Removed {
                        jid: dm_jid.to_string(),
                    },
                    // A retract of an already-absent item returns
                    // `item-not-found`; that's the desired end-state
                    // (no override), so treat it as success.
                    Err(stanza_err) if stanza_err.condition == "item-not-found" => {
                        WaddleSetDmNotificationModeOutcome::Removed {
                            jid: dm_jid.to_string(),
                        }
                    }
                    Err(stanza_err) => {
                        tracing::warn!(
                            condition = %stanza_err.condition,
                            "DM-bookmark retract rejected with stanza error",
                        );
                        WaddleSetDmNotificationModeOutcome::Error
                    }
                };
                return to_js_value(&outcome);
            }

            let publish_iq =
                build_publish_dm_bookmark_iq(&dm_jid, &merged, &uuid::Uuid::new_v4().to_string());
            let outcome = match send_iq_command_stanza_aware(inner, publish_iq).await? {
                Ok(_) => WaddleSetDmNotificationModeOutcome::Ok {
                    item: surface_dm_bookmark(&dm_jid, &merged),
                },
                Err(stanza_err) if stanza_err.condition == "precondition-not-met" => {
                    WaddleSetDmNotificationModeOutcome::NodeConfigMismatch
                }
                Err(stanza_err) => {
                    tracing::warn!(
                        condition = %stanza_err.condition,
                        "DM-bookmark publish rejected with stanza error",
                    );
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
    // A bookmark may (malformed-but-possible, folded by the merge code)
    // carry more than one `<notify/>` sibling, and a `<notify/>` may
    // hold only identity-scoped settings with no fallback. Scan ALL
    // notify children rather than stopping at the first: take the first
    // fallback mode found, and treat the opt-in as set if ANY notify
    // sibling carries the rich-payload marker — matching how both
    // `read_fallback_mode` and the server's `parse_rich_payload_opt_in`
    // resolve across siblings. Round-2 PR #804 review (Codex P2).
    let notifies = || {
        item.extensions.iter().filter(|ext| {
            ext.is(
                "notify",
                waddle_xmpp_client::xep::xep0492::NS_NOTIFICATION_SETTINGS,
            )
        })
    };
    let notify_mode = notifies().find_map(read_fallback_mode);
    let rich_payload_opt_in = notifies().any(read_rich_payload_opt_in);
    WaddleBookmarkItem {
        jid: item.jid.to_string(),
        name: item.name,
        autojoin: item.autojoin,
        notify_mode,
        rich_payload_opt_in,
    }
}

/// Extract each `(contact bare JID, <dm-bookmark> payload)` pair from a
/// XEP-0060 items response targeting the Waddle DM-bookmark node
/// (issue #720). Items with an unparseable id or a missing/malformed
/// `<dm-bookmark>` payload are skipped. The id is the contact's bare
/// JID (the PEP item id); the payload is the carrier element directly
/// hosting one XEP-0492 `<notify/>`.
///
/// Returns the typed payloads so callers can read the hosted `<notify/>`
/// (`set_dm_notification_mode` merges into it;
/// `parse_dm_bookmarks_response` surfaces it).
fn parse_dm_bookmark_payloads(iq: &Element) -> Vec<(jid::BareJid, Element)> {
    let Some(items) = iq
        .get_child("pubsub", waddle_xmpp_client::pep::NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", waddle_xmpp_client::pep::NS_PUBSUB))
        .filter(|items| items.attr("node") == Some(NS_WADDLE_DM_BOOKMARKS))
    else {
        return Vec::new();
    };
    items
        .children()
        .filter(|child| child.name() == "item" && child.ns() == waddle_xmpp_client::pep::NS_PUBSUB)
        .filter_map(|item| {
            let jid: jid::BareJid = item.attr("id")?.parse().ok()?;
            // The DM carrier contract requires the item id to be a
            // contact bare JID WITH a localpart (`localpart@domain`).
            // Skip a domain-only id so the chat never surfaces an
            // impossible DM entry — mirrors the server-side
            // `parse_dm_bookmark` localpart requirement (Copilot review).
            jid.node()?;
            let payload = item.get_child("dm-bookmark", NS_WADDLE_DM_BOOKMARKS)?;
            Some((jid, payload.clone()))
        })
        .collect()
}

/// Parse a DM-bookmark items response into the JS-facing surface
/// (issue #720). Reads each payload's hosted `<notify/>` for the
/// XEP-0492 fallback mode + rich-payload opt-in (#719). A payload
/// missing its `<notify/>` yields `notify_mode = None` (the chat
/// resolves it against the §3 direct-chat default).
fn parse_dm_bookmarks_response(iq: &Element) -> Vec<WaddleDmBookmarkItem> {
    parse_dm_bookmark_payloads(iq)
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

/// Surface one DM-bookmark `<notify/>` into the JS-facing
/// [`WaddleDmBookmarkItem`] (issue #720). Used by both
/// `fetch_dm_bookmarks` (per-item map) and `set_dm_notification_mode`
/// (response shaping of the merged `<notify/>`).
fn surface_dm_bookmark(jid: &jid::BareJid, notify: &Element) -> WaddleDmBookmarkItem {
    WaddleDmBookmarkItem {
        jid: jid.to_string(),
        notify_mode: read_fallback_mode(notify),
        rich_payload_opt_in: read_rich_payload_opt_in(notify),
    }
}

/// `CommandDriver` impl that forwards through the WASM client's
/// `send_iq_command` pipeline. The composer's typed `ClientResult`
/// surface meets the JS boundary's stringly-typed `JsValue` errors
/// here — any error from the WASM driver is wrapped as a typed
/// `StanzaError` so the composer's diagnostic chain stays consistent.
pub(crate) struct WasmCommandDriver {
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
}

impl waddle_xmpp_client::push::CommandDriver for WasmCommandDriver {
    async fn send_iq(
        &self,
        iq: Element,
    ) -> Result<Element, waddle_xmpp_client::error::ClientError> {
        send_iq_command(self.inner.clone(), iq).await.map_err(|js| {
            let text = js
                .as_string()
                .unwrap_or_else(|| "WASM send_iq failed".to_string());
            waddle_xmpp_client::error::ClientError::StanzaError(
                waddle_xmpp_client::error::StanzaError {
                    error_type: waddle_xmpp_client::error::StanzaErrorType::Cancel,
                    condition: "internal-server-error".to_string(),
                    text: Some(text),
                },
            )
        })
    }
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
mod tests {
    use super::*;

    #[test]
    fn verify_iq_from_accepts_exact_match() {
        assert!(verify_iq_from_matches_query(Some("push.example.com"), "push.example.com").is_ok());
    }

    #[test]
    fn verify_iq_from_rejects_absent_attr() {
        // RFC 6120 §8.1.2.1's absent-from permission only applies to
        // replies from the bound user's own server. The Push Service is
        // a separate XEP-0114 component, so absent-from is a server
        // violation we MUST NOT silently accept — a compromised C2S
        // could strip `from` instead of spoofing it.
        let err = verify_iq_from_matches_query(None, "push.example.com").unwrap_err();
        assert!(err.contains("push.example.com"));
        assert!(err.contains("no `from`"));
    }

    #[test]
    fn parse_dm_bookmark_payloads_skips_domain_only_item_id() {
        // A domain-only id (no localpart) is not a valid DM contact JID
        // — it must be skipped so the chat never surfaces an impossible
        // DM entry (Copilot review). A well-formed sibling still parses.
        let iq: Element = "<iq xmlns='jabber:client'>\
                <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                    <items node='urn:waddle:dm-bookmarks:0'>\
                        <item id='bob@example.com'>\
                            <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>\
                                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
                            </dm-bookmark>\
                        </item>\
                        <item id='example.com'>\
                            <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>\
                                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
                            </dm-bookmark>\
                        </item>\
                    </items>\
                </pubsub>\
            </iq>"
            .parse()
            .expect("valid iq");
        let parsed = parse_dm_bookmark_payloads(&iq);
        assert_eq!(parsed.len(), 1, "domain-only id must be skipped");
        assert_eq!(parsed[0].0.to_string(), "bob@example.com");
    }

    #[test]
    fn verify_iq_from_rejects_service_jid_with_resource() {
        // Push Service components don't carry resources (XEP-0114).
        // Accepting `push.example.com/anything` would be needlessly
        // permissive and wouldn't help any real-world deployment.
        let err =
            verify_iq_from_matches_query(Some("push.example.com/resource"), "push.example.com")
                .unwrap_err();
        assert!(err.contains("push.example.com/resource"));
    }

    #[test]
    fn verify_iq_from_accepts_case_variation_on_domain() {
        // RFC 6122 §2.4: domainpart comparison is case-insensitive.
        assert!(verify_iq_from_matches_query(Some("Push.Example.COM"), "push.example.com").is_ok());
    }

    #[test]
    fn verify_iq_from_rejects_unrelated_jid() {
        let err =
            verify_iq_from_matches_query(Some("attacker.tld"), "push.example.com").unwrap_err();
        assert!(err.contains("attacker.tld"));
        assert!(err.contains("push.example.com"));
    }

    #[test]
    fn verify_iq_from_rejects_prefix_substring_collision() {
        // `push.example.com` MUST NOT match `push.example.com.attacker.tld`
        // even via case-insensitive equality.
        assert!(verify_iq_from_matches_query(
            Some("push.example.com.attacker.tld"),
            "push.example.com",
        )
        .is_err());
    }

    #[test]
    fn verify_iq_from_rejects_empty_from() {
        let err = verify_iq_from_matches_query(Some(""), "push.example.com").unwrap_err();
        assert!(err.contains("push.example.com"));
    }

    #[test]
    fn surface_bookmark_reads_rich_payload_opt_in() {
        // #719 — the JS-facing item exposes the XEP-0492 §2.3
        // `<advanced><rich-payload xmlns='urn:waddle:push:rich:0'/>`
        // opt-in alongside the fallback notify mode.
        let extensions: Element = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <always>\
                        <advanced xmlns='urn:xmpp:notification-settings:1'>\
                            <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                        </advanced>\
                    </always>\
                </notify>\
            </extensions>"
            .parse()
            .expect("valid extensions");
        let item = BookmarkItem {
            jid: "room@muc.example.com".parse().expect("valid jid"),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: extensions.children().cloned().collect(),
        };
        let surfaced = surface_bookmark(item);
        assert!(surfaced.rich_payload_opt_in);
        assert_eq!(
            surfaced.notify_mode,
            Some(waddle_xmpp_client::xep::xep0492::NotifyMode::Always)
        );
    }

    #[test]
    fn surface_bookmark_opt_in_defaults_off_without_advanced() {
        let extensions: Element = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>\
            </extensions>"
            .parse()
            .expect("valid extensions");
        let item = BookmarkItem {
            jid: "room@muc.example.com".parse().expect("valid jid"),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: extensions.children().cloned().collect(),
        };
        let surfaced = surface_bookmark(item);
        assert!(!surfaced.rich_payload_opt_in);
    }

    #[test]
    fn surface_bookmark_scans_past_fallbackless_notify_sibling() {
        // Malformed-but-possible state the merge code explicitly folds:
        // multiple <notify/> siblings. The first carries only an
        // identity-scoped setting (no fallback, no rich marker); the
        // user's real fallback + rich opt-in live in the second. We
        // must scan ALL notify siblings, not stop at the first.
        let extensions: Element = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <never identity-category='client' identity-type='pc' />\
                </notify>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <always>\
                        <advanced xmlns='urn:xmpp:notification-settings:1'>\
                            <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                        </advanced>\
                    </always>\
                </notify>\
            </extensions>"
            .parse()
            .expect("valid extensions");
        let item = BookmarkItem {
            jid: "room@muc.example.com".parse().expect("valid jid"),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: extensions.children().cloned().collect(),
        };
        let surfaced = surface_bookmark(item);
        assert_eq!(
            surfaced.notify_mode,
            Some(waddle_xmpp_client::xep::xep0492::NotifyMode::Always)
        );
        assert!(surfaced.rich_payload_opt_in);
    }
}
