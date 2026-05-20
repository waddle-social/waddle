use super::*;

pub(crate) struct MucJoinPresence<'a> {
    pub(crate) occupant_id_secret: &'a waddle_xmpp::xep::xep0421::OccupantIdSecret,
    pub(crate) room_jid: &'a BareJid,
    pub(crate) nick: &'a str,
    pub(crate) to_jid: &'a FullJid,
    pub(crate) affiliation: Affiliation,
    pub(crate) role: Role,
    pub(crate) real_jid: &'a FullJid,
    pub(crate) include_self_status: bool,
    /// Optional `<call xmlns='urn:waddle:muc-call:0'/>` payload to
    /// append to the resulting presence stanza. Used by the join-
    /// replay path to surface "in call" indicators for occupants
    /// who were already participating in the room's group call
    /// before the joiner arrived. `None` for joiners' own
    /// self-presence and for occupants without an active call.
    pub(crate) call_extension: Option<&'a waddle_xmpp::xep::xep_waddle_muc_call::MucCallExtension>,
}

pub(crate) fn build_muc_join_presence_xml(params: MucJoinPresence<'_>) -> String {
    let presence = build_muc_join_presence_stanza(params);
    stanza_to_xml(&Stanza::Presence(presence))
}

pub(super) fn build_muc_join_presence_stanza(
    params: MucJoinPresence<'_>,
) -> xmpp_parsers::presence::Presence {
    let from_jid = params
        .room_jid
        .clone()
        .with_resource_str(params.nick)
        .unwrap_or_else(|_| params.to_jid.clone());
    let real_bare = params.real_jid.to_bare();
    let mut presence = waddle_xmpp::muc::build_occupant_presence(
        &from_jid,
        params.to_jid,
        params.affiliation,
        params.role,
        params.include_self_status,
        &waddle_xmpp::xep::xep0421::OccupantIdentity {
            bare_jid: &real_bare,
            real_jid: Some(params.real_jid),
            secret: params.occupant_id_secret,
        },
    );
    if let Some(ext) = params.call_extension {
        // The presence stanza already carries the server-authoritative
        // `<x xmlns='muc#user'>` + XEP-0421 occupant-id payloads from
        // `build_occupant_presence`; the `<call/>` extension is an
        // additional namespaced child per XEP-0045 §5.1.3 ("any other
        // extension element may be attached"). Built via the typed
        // `MucCallExtension::to_element` helper — never with
        // `format!`-style XML concat (CLAUDE.md XML hard rule).
        presence.payloads.push(ext.to_element());
    }
    presence
}

/// XEP-0045 §7.2.9 conflict presence: the requested nick is already in use
/// by a different user. The joiner receives a `<presence type='error'/>` and
/// no room state changes.
pub(super) fn build_muc_conflict_presence_xml(
    room_jid: &BareJid,
    nick: &str,
    to_jid: &FullJid,
) -> String {
    build_muc_presence_error_xml(
        room_jid,
        nick,
        to_jid,
        StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::Conflict,
            "en",
            "Nickname is already in use by another occupant.",
        ),
    )
}

/// Build a `<presence type='error'>` MUC join failure response.
///
/// Per the typed-payloads hard rule (CLAUDE.md), the error type and
/// condition flow as a typed
/// [`xmpp_parsers::stanza_error::StanzaError`] — never as `&str`. The
/// `<error/>` element is serialised via the upstream
/// `From<StanzaError> for Element` impl so the wire shape is identical
/// to the legacy `Element::builder("error", …)` literal.
pub(super) fn build_muc_presence_error_xml(
    room_jid: &BareJid,
    nick: &str,
    to_jid: &FullJid,
    error: StanzaError,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                from_jid.to_string(),
            )
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                to_jid.to_string(),
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error")
            .append(Element::from(error))
            .build(),
    )
}

pub(super) fn build_muc_self_unavailable_xml(
    state: &WebSocketState,
    room_jid: &BareJid,
    nick: &str,
    sender_jid: &FullJid,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| sender_jid.clone());

    let sender_bare = sender_jid.to_bare();
    let presence = waddle_xmpp::muc::build_leave_presence(
        &from_jid,
        sender_jid,
        Affiliation::Member,
        true,
        &waddle_xmpp::xep::xep0421::OccupantIdentity {
            bare_jid: &sender_bare,
            real_jid: Some(sender_jid),
            secret: &state.deps.occupant_id_secret,
        },
    );
    stanza_to_xml(&Stanza::Presence(presence))
}

/// Create a presence stanza for MUC. Used by the join-broadcast
/// path to announce a new occupant to existing occupants — that
/// new occupant has no `<call/>` advertisement yet, so the
/// extension slot is always `None` here.
pub(super) fn create_presence_stanza(
    state: &WebSocketState,
    room_jid: &BareJid,
    nick: &str,
    real_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    role: Role,
) -> xmpp_parsers::presence::Presence {
    build_muc_join_presence_stanza(MucJoinPresence {
        occupant_id_secret: &state.deps.occupant_id_secret,
        room_jid,
        nick,
        to_jid,
        affiliation,
        role,
        real_jid,
        include_self_status: false,
        call_extension: None,
    })
}
