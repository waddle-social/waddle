use jid::{BareJid, Jid};
use tracing::warn;
use waddle_extensions::{host_tools as ext_host, DisplayText};
use waddle_xmpp::{
    mam::ArchivedMessage as MamArchivedMessage,
    roster::{AskType, RosterItem, Subscription},
};
use xmpp_parsers::presence::Show;

use super::{
    ExtensionHostAdapterError, HostMucAffiliation, HostMucRole, HostPresenceShow, HostRosterAsk,
    HostRosterItem, HostRosterSubscription,
};

pub(super) fn host_tool_error(error: ExtensionHostAdapterError) -> ext_host::HostToolError {
    let code = match error {
        ExtensionHostAdapterError::NotAuthorized => ext_host::HostToolErrorCode::Denied,
        ExtensionHostAdapterError::RoomNotFound(_) => ext_host::HostToolErrorCode::NotFound,
        ExtensionHostAdapterError::Unsupported(_) => ext_host::HostToolErrorCode::Unsupported,
        ExtensionHostAdapterError::RoomActor(_)
        | ExtensionHostAdapterError::RoomOwnershipUncertain(_)
        | ExtensionHostAdapterError::Storage(_)
        | ExtensionHostAdapterError::Protocol(_) => ext_host::HostToolErrorCode::TemporaryFailure,
    };
    ext_host::HostToolError {
        code,
        message: DisplayText::new(error.to_string()).unwrap_or_else(|_| {
            DisplayText::new("extension host tool failed").expect("static text")
        }),
    }
}

pub(super) fn ext_muc_affiliation(affiliation: HostMucAffiliation) -> ext_host::MucAffiliation {
    match affiliation {
        HostMucAffiliation::Owner => ext_host::MucAffiliation::Owner,
        HostMucAffiliation::Admin => ext_host::MucAffiliation::Admin,
        HostMucAffiliation::Member => ext_host::MucAffiliation::Member,
        HostMucAffiliation::Outcast => ext_host::MucAffiliation::Outcast,
        HostMucAffiliation::None => ext_host::MucAffiliation::None,
    }
}

pub(super) fn ext_muc_role(role: HostMucRole) -> ext_host::MucRole {
    match role {
        HostMucRole::Moderator => ext_host::MucRole::Moderator,
        HostMucRole::Participant => ext_host::MucRole::Participant,
        HostMucRole::Visitor => ext_host::MucRole::Visitor,
        HostMucRole::None => ext_host::MucRole::None,
    }
}

pub(super) fn ext_presence_show(show: HostPresenceShow) -> Option<ext_host::PresenceShow> {
    match show {
        HostPresenceShow::Available => None,
        HostPresenceShow::Chat => Some(ext_host::PresenceShow::Chat),
        HostPresenceShow::Away => Some(ext_host::PresenceShow::Away),
        HostPresenceShow::Dnd => Some(ext_host::PresenceShow::DoNotDisturb),
        HostPresenceShow::Xa => Some(ext_host::PresenceShow::ExtendedAway),
    }
}

pub(super) fn ext_roster_subscription(
    subscription: HostRosterSubscription,
) -> ext_host::RosterSubscription {
    match subscription {
        HostRosterSubscription::None => ext_host::RosterSubscription::None,
        HostRosterSubscription::To => ext_host::RosterSubscription::To,
        HostRosterSubscription::From => ext_host::RosterSubscription::From,
        HostRosterSubscription::Both => ext_host::RosterSubscription::Both,
        HostRosterSubscription::Remove => ext_host::RosterSubscription::Remove,
    }
}

pub(super) fn ext_archived_message(
    message: MamArchivedMessage,
) -> Option<ext_host::ArchivedMessage> {
    Some(ext_host::ArchivedMessage {
        stanza_id: waddle_extensions::StanzaId::new(message.id).ok()?,
        from: message.from,
        to: message.to,
        sent_at: message.timestamp,
        // Documented lossy boundary (Q8): `DisplayText` is a non-empty
        // newtype on the extension/Wasm surface - by design, "displayable
        // text" cannot be empty. So an archived `Some("")` body collapses
        // to `None` here, conflating "wire `<body></body>`" with "no
        // `<body>` element" at this surface only. Wire fidelity for the
        // empty-vs-absent distinction is preserved upstream in
        // `MamArchivedMessage.body: Option<String>` and in the verbatim
        // `stanza_xml` column, which extensions can opt into via the raw
        // stanza surface if they need that distinction.
        body: message.body.and_then(|value| DisplayText::new(value).ok()),
        thread_id: message
            .thread
            .as_ref()
            .and_then(|t| waddle_extensions::ThreadId::new(t.id.as_str()).ok()),
        reply_to: message.reply.and_then(|reply| {
            // XEP-0461 §3 allows `<reply to=...>` to be either a bare or a
            // full JID. The extension/Wasm surface (`ReplyTarget.to`) is
            // typed `Option<FullJidValue>` and cannot currently carry a
            // bare JID - widening the WIT interface is out of scope for
            // PR #331 and would be an extension-API breaking change. As a
            // documented lossy boundary, we drop the `to` field for bare
            // JIDs but log a warning so the data loss is observable. The
            // reply itself (the stanza id) still flows through. Wire
            // fidelity is preserved upstream in `MamArchivedMessage.reply`
            // and `stanza_xml`.
            // TODO(#228 follow-up): widen the WIT `reply-target.to` to a
            // bare-or-full JID union so XEP-0461 1:1 replies addressed by
            // bare JID survive the extension boundary.
            let to = reply.to.and_then(|jid| {
                let jid_string = jid.to_string();
                match waddle_extensions::FullJidValue::new(jid_string.clone()) {
                    Ok(value) => Some(value),
                    Err(_) => {
                        warn!(
                            reply_to = %jid_string,
                            reply_id = %reply.id.as_str(),
                            "extension boundary: dropping bare-JID reply.to (FullJidValue requires a resource); see XEP-0461 §3"
                        );
                        None
                    }
                }
            });
            Some(waddle_extensions::ReplyTarget {
                id: waddle_extensions::StanzaId::new(reply.id.as_str().to_string()).ok()?,
                to,
            })
        }),
    })
}

pub(super) fn host_muc_affiliation(affiliation: waddle_xmpp::Affiliation) -> HostMucAffiliation {
    match affiliation {
        waddle_xmpp::Affiliation::Owner => HostMucAffiliation::Owner,
        waddle_xmpp::Affiliation::Admin => HostMucAffiliation::Admin,
        waddle_xmpp::Affiliation::Member => HostMucAffiliation::Member,
        waddle_xmpp::Affiliation::Outcast => HostMucAffiliation::Outcast,
        waddle_xmpp::Affiliation::None => HostMucAffiliation::None,
    }
}

pub(super) fn host_muc_role(role: waddle_xmpp::Role) -> HostMucRole {
    match role {
        waddle_xmpp::Role::Moderator => HostMucRole::Moderator,
        waddle_xmpp::Role::Participant => HostMucRole::Participant,
        waddle_xmpp::Role::Visitor => HostMucRole::Visitor,
        waddle_xmpp::Role::None => HostMucRole::None,
    }
}

pub(super) fn envelope_has_cross_room_launch(
    envelope: &waddle_extensions::ExtensionEnvelope,
    room: &BareJid,
) -> bool {
    let room = room.to_string();
    envelope.enrichments.iter().any(|enrichment| {
        enrichment.launches.iter().any(|launch| {
            launch
                .context
                .room
                .as_ref()
                .is_some_and(|launch_room| launch_room.as_str() != room)
        })
    })
}

pub(super) fn envelope_has_roomless_launch(
    envelope: &waddle_extensions::ExtensionEnvelope,
) -> bool {
    envelope.enrichments.iter().any(|enrichment| {
        enrichment
            .launches
            .iter()
            .any(|launch| launch.context.room.is_none())
    })
}

pub(super) fn host_presence_show(show: Show) -> HostPresenceShow {
    match show {
        Show::Chat => HostPresenceShow::Chat,
        Show::Away => HostPresenceShow::Away,
        Show::Dnd => HostPresenceShow::Dnd,
        Show::Xa => HostPresenceShow::Xa,
    }
}

pub(super) fn host_presence_show_str(show: &str) -> HostPresenceShow {
    match show {
        "chat" => HostPresenceShow::Chat,
        "away" => HostPresenceShow::Away,
        "dnd" => HostPresenceShow::Dnd,
        "xa" => HostPresenceShow::Xa,
        _ => HostPresenceShow::Available,
    }
}

pub(super) fn host_roster_item(item: RosterItem) -> HostRosterItem {
    HostRosterItem {
        jid: item.jid,
        name: item.name,
        subscription: match item.subscription {
            Subscription::None => HostRosterSubscription::None,
            Subscription::To => HostRosterSubscription::To,
            Subscription::From => HostRosterSubscription::From,
            Subscription::Both => HostRosterSubscription::Both,
            Subscription::Remove => HostRosterSubscription::Remove,
        },
        ask: item.ask.map(|ask| match ask {
            AskType::Subscribe => HostRosterAsk::Subscribe,
        }),
        groups: item.groups,
    }
}

pub(super) fn occupant_jid(room: &BareJid, nick: &str) -> Result<Jid, ExtensionHostAdapterError> {
    format!("{room}/{nick}")
        .parse()
        .map_err(|error: jid::Error| ExtensionHostAdapterError::Protocol(error.to_string()))
}
