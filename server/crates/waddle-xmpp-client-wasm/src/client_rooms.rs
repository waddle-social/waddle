use super::*;

/// In-call presence sub-state flags (`urn:waddle:in-call:0`) the chat
/// passes to [`WaddleClient::update_muji_presence`] as a plain JS object
/// `{ handRaised, muted }`. Bundled into one FFI argument so the call
/// presence carries both the #1029 raised hand and the #1030 mute — which
/// share the single `<in-call>` presence child — without re-spelling each
/// sub-state at the wasm boundary. Missing keys default to `false`.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InCallPresenceFlags {
    pub hand_raised: bool,
    pub muted: bool,
}

impl InCallPresenceFlags {
    /// Legacy callers (pre-#1030, when the FFI argument was the bare
    /// #1029 raised-hand boolean) may still pass `in_call` as a plain
    /// `boolean`. Interpret it as the raised-hand flag with mute
    /// cleared, so an older chat bundle never blocks the presence send.
    fn from_legacy_bool(hand_raised: bool) -> Self {
        Self {
            hand_raised,
            muted: false,
        }
    }
}

/// Representation-agnostic decision core of [`parse_in_call_flags`],
/// split out from the `JsValue` probing so the branch precedence and the
/// never-fails fallback are unit-testable without constructing a
/// `JsValue` (which aborts under native `cargo test`). Precedence:
///
/// 1. `nullish` (`undefined`/`null`) → [`InCallPresenceFlags::default()`].
/// 2. `legacy_bool` (a bare JS boolean) →
///    [`InCallPresenceFlags::from_legacy_bool`].
/// 3. otherwise the `deserialized` object, or `default()` if it was
///    `None` (deserialization failed) — this is the guarantee that the
///    parse NEVER fails.
fn resolve_in_call_flags(
    nullish: bool,
    legacy_bool: Option<bool>,
    deserialized: Option<InCallPresenceFlags>,
) -> InCallPresenceFlags {
    if nullish {
        return InCallPresenceFlags::default();
    }
    if let Some(hand_raised) = legacy_bool {
        return InCallPresenceFlags::from_legacy_bool(hand_raised);
    }
    deserialized.unwrap_or_default()
}

/// Best-effort parse of the `in_call` FFI argument into
/// [`InCallPresenceFlags`]. This NEVER fails: presence sends — and in
/// particular the call-teardown leave marker that clears `<muji/>` for
/// remote occupants — must not be blocked by a malformed or stale
/// `in_call` value. Each non-object shape degrades to a sensible flag set
/// rather than a rejected Promise (see [`resolve_in_call_flags`]):
///
/// - `undefined` / `null` → [`InCallPresenceFlags::default()`] (both flags
///   cleared); this is the shape leave/teardown paths want anyway.
/// - a bare `boolean` (legacy caller) →
///   [`InCallPresenceFlags::from_legacy_bool`].
/// - anything else → deserialize the `{ handRaised, muted }` object,
///   falling back to `default()` if deserialization fails for any reason.
///   (A *partially* malformed object — e.g. a non-boolean field value —
///   fails as a whole and defaults every flag, not just the bad one.)
fn parse_in_call_flags(in_call: JsValue) -> InCallPresenceFlags {
    // Probe the `JsValue` shape before the move-consuming deserialize.
    let nullish = in_call.is_undefined() || in_call.is_null();
    let legacy_bool = in_call.as_bool();
    let deserialized = serde_wasm_bindgen::from_value(in_call).ok();
    resolve_in_call_flags(nullish, legacy_bool, deserialized)
}

#[wasm_bindgen]
impl WaddleClient {
    /// Join a MUC room. Always requests zero discussion history
    /// (`<history maxstanzas='0'/>`, XEP-0045 §7.2.15): MAM catch-up is
    /// the authoritative history source, so accepting the service's
    /// default join history would double-deliver recent messages (#1255).
    /// The canonical presence shape lives in
    /// [`waddle_xmpp_client::messaging::build_muc_join_presence`].
    pub fn join_room(&self, room_jid: String, nick: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            // Parse untyped FFI input into a typed JID once, here at the
            // boundary (typed-payloads rule).
            let room: BareJid = room_jid
                .parse()
                .map_err(|err| js_error(format!("invalid room JID: {err}")))?;
            let stanza = waddle_xmpp_client::messaging::build_muc_join_presence(&room, &nick)
                .map_err(|err| js_error(err.to_string()))?;
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
    /// - `in_call`: a plain JS object `{ handRaised, muted }`
    ///   ([`InCallPresenceFlags`]) appended as an `<in-call
    ///   xmlns='urn:waddle:in-call:0'>` presence child *alongside*
    ///   `<muji/>` (#1029 raised hand / #1030 mute), one marker child per
    ///   set flag. This is the FFI in-call "set method": the caller
    ///   re-emits its current call presence with the flags toggled, and
    ///   the absence of a marker clears that sub-state for everyone (the
    ///   server drops the stored state). Ignored unless the occupant is in
    ///   the call (`active` or `preparing`), since in-call state is
    ///   meaningless without call participation.
    pub fn update_muji_presence(
        &self,
        room_jid: String,
        nick: String,
        active: bool,
        preparing: bool,
        video: bool,
        // Tighten the generated `.d.ts` from `any` to the `{ handRaised,
        // muted }` shape so TypeScript callers can't silently pass a
        // malformed value. Both keys are optional because the parse
        // defaults any missing flag (see `parse_in_call_flags`).
        #[wasm_bindgen(unchecked_param_type = "{ handRaised?: boolean; muted?: boolean }")]
        in_call: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            // Best-effort: a malformed / undefined / legacy-boolean
            // `in_call` MUST NOT block the presence send (notably the
            // call-teardown leave marker), so this degrades to sensible
            // flags instead of rejecting the Promise.
            let in_call = parse_in_call_flags(in_call);
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
                if in_call.hand_raised || in_call.muted {
                    builder = builder.append(
                        waddle_xmpp_client::messaging::build_in_call_presence_state_element(
                            in_call.hand_raised,
                            in_call.muted,
                        ),
                    );
                }
            }
            builder = builder.append(waddle_xmpp_client::caps::build_client_caps_element());
            send_stanza_command(inner, builder.build()).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Publish the user's own presence (RFC 6121 §4.7). `show` is an RFC
    /// 6121 `<show>` value (`away` / `xa` / `dnd` / `chat`) or `None` for
    /// plain Available (the absence of a `<show>`); `status` is the optional
    /// free-text line; `idle_since` is the XEP-0319 last-interaction instant as
    /// an xs:dateTime string (auto-away only), or `None` on a return-from-idle.
    /// Shares `build_presence_stanza` with the native + FFI `send_presence`, so
    /// every Waddle client emits byte-identical presence including the XEP-0115
    /// caps advertisement.
    pub fn send_presence(
        &self,
        status: Option<String>,
        show: Option<String>,
        idle_since: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            // Parse the untyped xs:dateTime once here (typed-payloads rule);
            // an unparseable value degrades to no idle rather than wire junk.
            let presence = waddle_xmpp_client::messaging::build_presence_stanza(
                status.as_deref(),
                show.as_deref()
                    .and_then(waddle_xmpp_client::messaging::parse_show),
                idle_since.and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok()),
            );
            send_stanza_command(inner, presence.into()).await?;
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

#[cfg(test)]
mod tests {
    use super::{resolve_in_call_flags, InCallPresenceFlags};

    // `parse_in_call_flags` itself takes a `JsValue`, and
    // `wasm_bindgen::JsValue` panics under `cargo test` on native
    // targets (it's only safe on `wasm32`). So we test the
    // representation-independent pieces it composes — the legacy-bool
    // mapping, the serde shape its `serde_wasm_bindgen::from_value` path
    // relies on, and the `resolve_in_call_flags` decision core (branch
    // precedence + never-fails fallback) — directly, with `serde_json`
    // standing in for `serde_wasm_bindgen` (both drive the same
    // `Deserialize` impl).

    #[test]
    fn resolve_nullish_clears_all_flags_even_over_other_inputs() {
        // `undefined`/`null` is the leave/teardown shape and wins
        // ahead of every other branch.
        let flags = resolve_in_call_flags(
            true,
            Some(true),
            Some(InCallPresenceFlags {
                hand_raised: true,
                muted: true,
            }),
        );
        assert!(!flags.hand_raised);
        assert!(!flags.muted);
    }

    #[test]
    fn resolve_legacy_bool_takes_precedence_over_deserialized_object() {
        // A bare boolean is the legacy raised-hand carrier; it is
        // resolved before the object path.
        let flags = resolve_in_call_flags(
            false,
            Some(true),
            Some(InCallPresenceFlags {
                hand_raised: false,
                muted: true,
            }),
        );
        assert!(flags.hand_raised);
        assert!(!flags.muted);
    }

    #[test]
    fn resolve_uses_deserialized_object_when_no_legacy_bool() {
        let flags = resolve_in_call_flags(
            false,
            None,
            Some(InCallPresenceFlags {
                hand_raised: true,
                muted: true,
            }),
        );
        assert!(flags.hand_raised);
        assert!(flags.muted);
    }

    #[test]
    fn resolve_falls_back_to_default_when_deserialization_failed() {
        // The never-reject guarantee: a value that is neither nullish
        // nor a bool and that failed to deserialize (e.g. a number, a
        // string, or a type-mismatched object) MUST still resolve to
        // default flags so the presence is sent rather than blocked.
        let flags = resolve_in_call_flags(false, None, None);
        assert!(!flags.hand_raised);
        assert!(!flags.muted);
    }

    #[test]
    fn default_flags_clear_all_in_call_state() {
        // The leave-marker / undefined path falls back to this: a bare
        // call presence with neither hand nor mute marker, clearing both
        // sub-states for remote occupants.
        let flags = InCallPresenceFlags::default();
        assert!(!flags.hand_raised);
        assert!(!flags.muted);
    }

    #[test]
    fn legacy_bool_true_means_raised_hand_only() {
        // Pre-#1030 callers passed `in_call` as a bare boolean carrying
        // the #1029 raised-hand intent; mute is unknown to them, so it
        // stays cleared.
        let flags = InCallPresenceFlags::from_legacy_bool(true);
        assert!(flags.hand_raised);
        assert!(!flags.muted);
    }

    #[test]
    fn legacy_bool_false_clears_both() {
        let flags = InCallPresenceFlags::from_legacy_bool(false);
        assert!(!flags.hand_raised);
        assert!(!flags.muted);
    }

    #[test]
    fn empty_object_deserializes_to_default() {
        let flags: InCallPresenceFlags =
            serde_json::from_value(serde_json::json!({})).expect("empty object deserializes");
        assert!(!flags.hand_raised);
        assert!(!flags.muted);
    }

    #[test]
    fn partial_object_defaults_missing_keys() {
        let raised: InCallPresenceFlags =
            serde_json::from_value(serde_json::json!({ "handRaised": true }))
                .expect("partial object deserializes");
        assert!(raised.hand_raised);
        assert!(!raised.muted);

        let muted: InCallPresenceFlags =
            serde_json::from_value(serde_json::json!({ "muted": true }))
                .expect("partial object deserializes");
        assert!(!muted.hand_raised);
        assert!(muted.muted);
    }

    #[test]
    fn full_camel_case_object_deserializes_both_flags() {
        let flags: InCallPresenceFlags =
            serde_json::from_value(serde_json::json!({ "handRaised": true, "muted": true }))
                .expect("full object deserializes");
        assert!(flags.hand_raised);
        assert!(flags.muted);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // A forward/backward-incompatible caller can sneak extra keys in;
        // the lenient parse must still recover the flags it understands
        // rather than rejecting the whole presence.
        let flags: InCallPresenceFlags = serde_json::from_value(
            serde_json::json!({ "handRaised": true, "speaking": true, "extra": 7 }),
        )
        .expect("unknown keys are ignored");
        assert!(flags.hand_raised);
        assert!(!flags.muted);
    }
}
