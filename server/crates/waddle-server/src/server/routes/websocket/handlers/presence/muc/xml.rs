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
    waddle_xmpp::muc::build_occupant_presence(
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
    )
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
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "error")
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

/// Create a presence stanza for MUC
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
    })
}
