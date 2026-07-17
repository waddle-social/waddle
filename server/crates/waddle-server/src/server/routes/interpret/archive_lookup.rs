use super::*;

pub(super) fn waddle_id_for_room_jid(room_jid: &BareJid) -> WaddleId {
    let value = if parse_managed_room_jid(room_jid).is_some() {
        "space".to_string()
    } else {
        "default".to_string()
    };
    WaddleId::new(value).expect("static room Waddle scope is non-empty")
}

/// Resolve a typed [`MessageRef`] against the explicitly selected MAM
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
    archive_kind: waddle_xmpp::mam::MamArchiveKind,
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
            // Use the backend's bounded sender-owned lookup. It matches the
            // explicit origin-id first shape and the legacy wire `id`
            // fallback without applying XEP-0313 `with=owner` self-chat
            // semantics or scanning sequential MAM pages.
            mam_storage
                .get_message_by_sender_and_origin_id(archive, archive_kind, sender, origin_id)
                .await
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
    let tombstoned = row
        .rich
        .as_ref()
        .is_some_and(waddle_xmpp::mam::ArchivedRichMessage::is_tombstoned);

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
