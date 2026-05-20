use super::*;

pub(super) fn waddle_id_for_room_jid(room_jid: &BareJid) -> WaddleId {
    let value = if parse_managed_room_jid(room_jid).is_some() {
        "space".to_string()
    } else {
        "default".to_string()
    };
    WaddleId::new(value).expect("static room Waddle scope is non-empty")
}

/// Resolve a typed [`MessageRef`] against `archive`'s personal MAM
/// archive and project the storage row into the typed protocol
/// [`ProtocolArchivedMessage`] shape the
/// [`waddle_xmpp::protocol::handlers::rich_target_validation::RichTargetValidationHandler`]
/// expects on the [`InboundEvent::ArchivedMessageLoaded`] callback.
///
/// Storage failures are demoted to `Ok(None)` with a WARN log so the
/// handler treats them as `<item-not-found>` per XEP-0308 / 0424 /
/// 0425 / 0461 — the same surface clients see when the target
/// genuinely doesn't exist. We do not propagate the storage error
/// shape into the callback because the protocol-side type does not
/// model it and the resulting reply would be the same.
pub(super) async fn lookup_archived_message(
    deps: &Deps<'_>,
    archive: &jid::BareJid,
    reference: &MessageRef,
) -> Option<Box<ProtocolArchivedMessage>> {
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            archive = %archive,
            "LookupArchivedMessage: no mam_storage in Deps; treating as not-found"
        );
        return None;
    };
    let lookup = match reference {
        MessageRef::StanzaId { stanza_id } => {
            // Strict stanza-id match: `get_message_by_message_id`
            // matches only the `stanza_id` column (not `origin_id`)
            // so the OR-collision identified in #229 PR8 review
            // (origin-id colliding with someone else's stanza-id)
            // can't return the wrong row.
            mam_storage
                .get_message_by_message_id(archive, stanza_id.as_str())
                .await
        }
        MessageRef::OriginId { sender, origin_id } => {
            // No origin-id-only accessor on `MamStorage` today, so we
            // narrow with `MamQuery.with = sender` (storage-level
            // sender filter) and pick the first row whose `origin_id`
            // *or wire `id` attribute* (`stanza_id` column) matches
            // the requested value. The fall-through to the wire id
            // mirrors the legacy `lookup_correction_target_message`
            // behaviour — many clients (and the CUE e2e scenarios)
            // omit the explicit `<origin-id/>` payload and rely on
            // the message's `id` attribute as the correction
            // target. Sender-bound matching keeps the
            // OR-collision protection from #229 PR8: only rows sent
            // by the original author can satisfy the correction.
            let query = waddle_xmpp::mam::MamQuery {
                with: Some(jid::Jid::from(sender.clone())),
                ..Default::default()
            };
            match mam_storage.query_messages(archive, &query).await {
                Ok(result) => Ok(result.messages.into_iter().find(|row| {
                    row_matches_origin_id(row, sender, origin_id.as_str())
                        || row_matches_wire_id(row, sender, origin_id.as_str())
                })),
                Err(e) => Err(e),
            }
        }
    };
    match lookup {
        Ok(Some(row)) => project_archived_row(archive, row),
        Ok(None) => None,
        Err(error) => {
            warn!(
                archive = %archive,
                %error,
                "LookupArchivedMessage: storage error; treating as not-found"
            );
            None
        }
    }
}

/// Verify a storage row genuinely matches the typed
/// [`MessageRef::OriginId`] key — `origin_id` field equality plus
/// `sender` (`from`) equality via parsed [`jid::BareJid`] comparison.
/// String equality on `from` would mishandle case-folded localparts
/// or full-JID-vs-bare-JID strings; the typed compare normalizes both.
pub(super) fn row_matches_origin_id(
    row: &MamArchivedMessage,
    expected_sender: &jid::BareJid,
    expected_origin_id: &str,
) -> bool {
    if row.origin_id.as_ref().map(|o| o.id.as_str()) != Some(expected_origin_id) {
        return false;
    }
    row.from.to_bare() == *expected_sender
}

/// Fallback match for the legacy XEP-0308 correction-target shape
/// where the message carries no explicit `<origin-id/>` payload —
/// matches the row's wire `id` attribute (the `stanza_id` storage
/// column) to the correction's `replaces_id`. Same sender-bound
/// scoping as [`row_matches_origin_id`].
pub(super) fn row_matches_wire_id(
    row: &MamArchivedMessage,
    expected_sender: &jid::BareJid,
    expected_wire_id: &str,
) -> bool {
    if row.stanza_id.as_ref().map(|s| s.id.as_str()) != Some(expected_wire_id) {
        return false;
    }
    row.from.to_bare() == *expected_sender
}

/// Project a storage [`MamArchivedMessage`] row into the protocol-side
/// [`ProtocolArchivedMessage`] consumed by handler completions.
///
/// Falls back to an empty body-only stanza if `stanza_xml` is missing
/// or unparseable — the rich-target handler primarily inspects the
/// tombstone state and the sender's bare JID, both of which we can
/// reconstruct without the original wire form. Logs a WARN at each
/// projection failure mode so regressions stay observable.
pub(super) fn project_archived_row(
    archive: &jid::BareJid,
    row: MamArchivedMessage,
) -> Option<Box<ProtocolArchivedMessage>> {
    let tombstoned = matches!(
        row.rich.as_ref().and_then(|r| r.payload.as_ref()),
        Some(waddle_xmpp::mam::ArchivedRichPayload::Tombstone(_))
    );

    let message = match parse_archived_message_xml(row.stanza_xml.as_deref()) {
        Some(m) => m,
        None => fallback_archived_message(&row),
    };

    // XEP-0359 §3 / XEP-0313 §5.2: the archive's <stanza-id> MUST carry
    // the archive-stamped value (our schema's `row.id`), not the original
    // wire `<message id>`. Follow-up handlers target the archived entry
    // by this canonical id; preferring `row.stanza_id.id` (the wire id)
    // would resolve incorrectly when the two differ.
    let archive_jid: jid::Jid = archive.clone().into();
    let stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(&row.id, archive_jid);

    Some(Box::new(ProtocolArchivedMessage {
        stanza_id,
        message: Box::new(message),
        tombstoned,
    }))
}

pub(super) fn parse_archived_message_xml(xml: Option<&str>) -> Option<Message> {
    let xml = xml?;
    let element = match Element::from_str(xml) {
        Ok(e) => e,
        Err(error) => {
            warn!(
                %error,
                "LookupArchivedMessage: failed to parse stored stanza_xml; \
                 falling back to body-only reconstruction"
            );
            return None;
        }
    };
    let element_name = element.name().to_string();
    let element_ns = element.ns().to_string();
    match Message::try_from(element) {
        Ok(message) => Some(message),
        Err(error) => {
            warn!(
                %error,
                element_name = %element_name,
                element_ns = %element_ns,
                "LookupArchivedMessage: stored stanza_xml parsed but failed \
                 to convert into xmpp_parsers::message::Message; falling \
                 back to body-only reconstruction"
            );
            None
        }
    }
}

pub(super) fn fallback_archived_message(row: &MamArchivedMessage) -> Message {
    let mut msg = Message::new(Some(row.to.clone()));
    msg.from = Some(row.from.clone());
    // Preserve the archived row's MessageType. Without this, the
    // body-only fallback rebuilds via `Message::new` whose default
    // type would project groupchat rows back as the default type and
    // break downstream ownership/retraction logic that branches on
    // `msg.type_`.
    msg.type_ = row.message_type.clone();
    msg.id = row
        .stanza_id
        .as_ref()
        .map(|s| xmpp_parsers::message::Id(s.id.clone()));
    // RFC 6121 §5.2.3: only emit `<body>` if the archived row recorded
    // one. `Some("")` round-trips as an empty `<body></body>` element;
    // `None` produces no `<body>` element at all (subject-only,
    // reaction-only, etc.).
    if let Some(body) = row.body.as_deref() {
        msg.bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_owned());
    }
    msg
}

/// Build the XEP-0297-wrapped carbon envelope for `kind`. Pulled out
/// so the live-resources fan-out and the detached-XEP-0198 fan-out
/// share one builder.
pub(super) fn build_carbon_envelope(
    kind: CarbonKind,
    original: &xmpp_parsers::message::Message,
    owner_bare: &str,
    target_full: &jid::FullJid,
) -> Result<xmpp_parsers::message::Message, jid::Error> {
    let target = target_full.to_string();
    match kind {
        CarbonKind::Sent => build_sent_carbon(original, owner_bare, &target),
        CarbonKind::Received => build_received_carbon(original, owner_bare, &target),
    }
}

/// Helper trait so the interpreter has a single, typed serialization
/// entry point for any `Stanza` leaving the state machine. Keeping it
/// private to this module prevents callers from serializing stanzas in
/// other spots — the I/O boundary stays narrow.
pub(super) trait ToElementString {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError>;
}

impl ToElementString for waddle_xmpp::Stanza {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError> {
        use waddle_xmpp::Stanza;
        match self {
            Stanza::Iq(iq) => stanza_to_string(*iq.clone()),
            Stanza::Message(msg) => message_to_string(msg),
            Stanza::Presence(p) => stanza_to_string(p.clone()),
        }
    }
}
