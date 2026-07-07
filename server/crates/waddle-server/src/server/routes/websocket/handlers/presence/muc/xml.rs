use super::*;

pub(crate) struct MucJoinPresence<'a> {
    pub(crate) occupant_id_secret: &'a waddle_xmpp::xep::xep0421::OccupantIdSecret,
    pub(crate) room_jid: &'a BareJid,
    pub(crate) nick: &'a str,
    pub(crate) to_jid: &'a FullJid,
    pub(crate) affiliation: Affiliation,
    pub(crate) role: Role,
    pub(crate) real_jid: &'a FullJid,
    pub(crate) disclose_real_jid: bool,
    pub(crate) include_self_status: bool,
    pub(crate) room_created: bool,
    pub(crate) include_nonanonymous_status: bool,
    /// Optional XEP-0272 `<muji xmlns='urn:xmpp:jingle:muji:0'/>`
    /// payload to append to the resulting presence stanza. Used by
    /// the join-replay path to surface "in call" indicators for
    /// occupants who were already participating in the room's group
    /// call before the joiner arrived. `None` for joiners' own
    /// self-presence and for occupants without an active Muji
    /// advertisement.
    pub(crate) muji: Option<&'a waddle_xmpp::xep::xep0272::Muji>,
    /// In-call presence state (`urn:waddle:in-call:0`, #1029 raised hand
    /// / #1030 mute) to append as an `<in-call>` payload alongside
    /// `<muji/>`. Set for the join-replay path when the occupant being
    /// replayed advertises one, so a late joiner sees their hand/mute
    /// immediately. Empty (default) for joiners' own self-presence and
    /// for occupants advertising no in-call state — an empty state emits
    /// no `<in-call>` element.
    pub(crate) in_call: waddle_xmpp::xep::InCallPresenceState,
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
    let visible_real_jid = params.disclose_real_jid.then_some(params.real_jid);
    let mut presence = waddle_xmpp::muc::build_occupant_presence(
        &from_jid,
        params.to_jid,
        params.affiliation,
        params.role,
        waddle_xmpp::muc::MucPresenceStatus {
            is_self: params.include_self_status,
            room_created: params.room_created,
            include_nonanonymous_status: params.include_nonanonymous_status,
        },
        &waddle_xmpp::xep::xep0421::OccupantIdentity {
            bare_jid: &real_bare,
            real_jid: visible_real_jid,
            secret: params.occupant_id_secret,
        },
    );
    if let Some(muji) = params.muji {
        // The presence stanza already carries the server-authoritative
        // `<x xmlns='muc#user'>` + XEP-0421 occupant-id payloads from
        // `build_occupant_presence`; the `<muji/>` extension is an
        // additional namespaced child per XEP-0045 §5.1.3 ("any other
        // extension element may be attached") and XEP-0272 §Joining
        // (Muji presence shape). Built via the typed `Muji::to_element`
        // helper — never with `format!`-style XML concat (CLAUDE.md
        // XML hard rule).
        presence.payloads.push(muji.to_element());
    }
    if !params.in_call.is_empty() {
        // The `<in-call>` state (raised hand / mute) is a sibling of
        // `<muji>` (never nested), an additional namespaced presence
        // extension per XEP-0045 §5.1.3. Built via the typed carrier
        // helper, which emits one marker child per advertised sub-state.
        presence
            .payloads
            .push(waddle_xmpp::xep::build_in_call_presence_state_element(
                &params.in_call,
            ));
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
    mut error: StanzaError,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());
    // XEP-0045 §7.2 error examples stamp the room bare JID as the
    // erroring entity (`<error by='room@service'>`); preserve an
    // explicitly-set `by` should a caller ever provide one.
    error.by.get_or_insert_with(|| room_jid.clone().into());

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
            // XEP-0045 §7.2: join-failure presence errors echo the
            // `<x xmlns='http://jabber.org/protocol/muc'/>` element so
            // clients can associate the error with the join request.
            .append(Element::from(xmpp_parsers::muc::Muc::new()))
            .append(Element::from(error))
            .build(),
    )
}

pub(super) fn build_muc_self_unavailable_xml(
    state: &WebSocketState,
    room_jid: &BareJid,
    nick: &str,
    sender_jid: &FullJid,
    include_nonanonymous_status: bool,
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
        waddle_xmpp::muc::MucPresenceStatus::new(true, include_nonanonymous_status),
        &waddle_xmpp::xep::xep0421::OccupantIdentity {
            bare_jid: &sender_bare,
            real_jid: Some(sender_jid),
            secret: &state.deps.occupant_id_secret,
        },
    );
    stanza_to_xml(&Stanza::Presence(presence))
}
