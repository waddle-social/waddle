use tracing::{debug, warn};
use waddle_xmpp::{
    parser::stanza_to_string,
    protocol::handlers::errors::{bad_request_reply, message_error_reply},
    Stanza,
};
use waddle_xmpp_core::xep0359::StanzaId;

use crate::server::routes::websocket::WebSocketState;

pub(super) async fn handle_dm_pin_message(
    incoming: &xmpp_parsers::message::Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) -> Option<Vec<String>> {
    if incoming.type_ != xmpp_parsers::message::MessageType::Chat {
        return None;
    }
    let intent = match waddle_xmpp::xep::extract_pin_intent_from_message(incoming) {
        Some(intent) => intent,
        None => {
            if incoming
                .payloads
                .iter()
                .any(|payload| payload.ns() == waddle_xmpp::xep::NS_WADDLE_PIN_V0)
            {
                let mut stamped = incoming.clone();
                stamped.from = Some(jid::Jid::from(bound_jid.clone()));
                let reply = bad_request_reply(&stamped, "Malformed DM pin marker.");
                return stanza_to_string(reply).ok().map(|frame| vec![frame]);
            }
            return None;
        }
    };
    let target = intent.target().to_string();
    let Some(peer) = incoming.to.as_ref().map(|to| to.to_bare()) else {
        let mut stamped = incoming.clone();
        stamped.from = Some(jid::Jid::from(bound_jid.clone()));
        let reply = bad_request_reply(&stamped, "DM pin marker requires a local peer JID.");
        return stanza_to_string(reply).ok().map(|frame| vec![frame]);
    };
    if peer.domain() != bound_jid.domain() {
        let mut stamped = incoming.clone();
        stamped.from = Some(jid::Jid::from(bound_jid.clone()));
        let reply = message_error_reply(
            &stamped,
            xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Auth,
                xmpp_parsers::stanza_error::DefinedCondition::Forbidden,
                "en",
                "DM pins are only supported for local peers.",
            ),
        );
        return stanza_to_string(reply).ok().map(|frame| vec![frame]);
    }

    let sender = bound_jid.to_bare();
    let key = crate::server::routes::websocket::DmPairKey::new(sender.clone(), peer.clone());
    if matches!(intent, waddle_xmpp::xep::PinIntent::Unpin { .. }) {
        let Some(target) =
            lookup_dm_pin_target(state, [&peer, &sender], &sender, &peer, &target).await
        else {
            return Some(Vec::new());
        };
        if !state
            .deps
            .protocol
            .dm_pin_store
            .unpin(&key, &target.canonical_stanza_id)
        {
            return Some(Vec::new());
        }
        let event = build_dm_pin_event_message(
            &sender,
            &peer,
            DmPinAction::Unpinned,
            &target.canonical_stanza_id,
            &sender,
            None,
        );
        fanout_dm_pin_event(state, &sender, &[sender.clone(), peer], event).await;
        return Some(Vec::new());
    }
    let target = match lookup_dm_pin_target(state, [&peer, &sender], &sender, &peer, &target).await
    {
        Some(found) => found,
        None => {
            let mut stamped = incoming.clone();
            stamped.from = Some(jid::Jid::from(bound_jid.clone()));
            let reply = message_error_reply(
                &stamped,
                xmpp_parsers::stanza_error::StanzaError::new(
                    xmpp_parsers::stanza_error::ErrorType::Cancel,
                    xmpp_parsers::stanza_error::DefinedCondition::ItemNotFound,
                    "en",
                    "Pinned DM target was not found.",
                ),
            );
            return stanza_to_string(reply).ok().map(|frame| vec![frame]);
        }
    };
    let body = target.archived.body.as_deref().unwrap_or("");
    let preview = waddle_xmpp::muc::PinPreview::new(
        target.archived.from.to_bare(),
        None,
        body,
        target.archived.timestamp,
    );
    let target_stanza_id = target.canonical_stanza_id.clone();
    let entry = waddle_xmpp::muc::PinnedEntry {
        target_stanza_id: target_stanza_id.clone(),
        pinner_jid: sender.clone(),
        pinned_at: chrono::Utc::now(),
        preview,
    };
    state
        .deps
        .protocol
        .dm_pin_store
        .apply_pin(key, entry.clone());

    let event = build_dm_pin_event_message(
        &sender,
        &peer,
        DmPinAction::Pinned,
        &entry.target_stanza_id,
        &entry.pinner_jid,
        Some(&entry.preview),
    );
    fanout_dm_pin_event(state, &sender, &[sender.clone(), peer], event).await;
    Some(Vec::new())
}

async fn lookup_dm_pin_target<'a>(
    state: &WebSocketState,
    archives: impl IntoIterator<Item = &'a jid::BareJid>,
    sender: &jid::BareJid,
    peer: &jid::BareJid,
    target: &str,
) -> Option<DmPinTarget> {
    for archive in archives {
        match state
            .deps
            .protocol
            .mam_storage
            .get_message_by_archive_or_stanza_id(archive, target)
            .await
        {
            Ok(Some(archived))
                if archived_belongs_to_dm_pair(&archived, sender, peer)
                    && !is_tombstoned_archive_row(&archived) =>
            {
                let canonical_stanza_id = canonical_dm_pin_stanza_id(&archived, archive);
                return Some(DmPinTarget {
                    canonical_stanza_id,
                    archived,
                });
            }
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(error) => {
                warn!(
                    archive = %archive,
                    target,
                    %error,
                    "DM pin target lookup failed"
                );
            }
        }
    }
    None
}

struct DmPinTarget {
    canonical_stanza_id: StanzaId,
    archived: waddle_xmpp::mam::ArchivedMessage,
}

fn canonical_dm_pin_stanza_id(
    archived: &waddle_xmpp::mam::ArchivedMessage,
    archive_jid: &jid::BareJid,
) -> StanzaId {
    let by = jid::Jid::from(archive_jid.clone());
    if let Some(stanza_id) = archived.stanza_id.as_ref() {
        return StanzaId::new(stanza_id.id.clone(), by);
    }
    if let Some(origin_id) = archived.origin_id.as_ref() {
        return StanzaId::new(origin_id.id.clone(), by);
    }
    StanzaId::new(archived.id.clone(), by)
}

fn is_tombstoned_archive_row(archived: &waddle_xmpp::mam::ArchivedMessage) -> bool {
    archived
        .rich
        .as_ref()
        .is_some_and(waddle_xmpp::mam::ArchivedRichMessage::is_tombstoned)
}

fn archived_belongs_to_dm_pair(
    archived: &waddle_xmpp::mam::ArchivedMessage,
    sender: &jid::BareJid,
    peer: &jid::BareJid,
) -> bool {
    let from = archived.from.to_bare();
    let to = archived.to.to_bare();
    (from == *sender && to == *peer) || (from == *peer && to == *sender)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmPinAction {
    Pinned,
    Unpinned,
}

impl DmPinAction {
    fn as_attr(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Unpinned => "unpinned",
        }
    }

    fn body_verb(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Unpinned => "unpinned",
        }
    }
}

fn build_dm_pin_event_message(
    sender: &jid::BareJid,
    peer: &jid::BareJid,
    action: DmPinAction,
    target: &StanzaId,
    by: &jid::BareJid,
    preview: Option<&waddle_xmpp::muc::PinPreview>,
) -> xmpp_parsers::message::Message {
    let mut event = xmpp_parsers::message::Message::new(Some(jid::Jid::from(peer.clone())));
    event.from = Some(jid::Jid::from(sender.clone()));
    event.type_ = xmpp_parsers::message::MessageType::Chat;
    event.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        format!("{by} {} a message", action.body_verb()),
    );
    let mut pin_event = minidom::Element::builder("pin-event", waddle_xmpp::xep::NS_WADDLE_PIN_V0)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            action.as_attr(),
        )
        .attr(
            minidom::rxml::xml_ncname!("target").to_owned(),
            target.id.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("by").to_owned(),
            by.to_string().as_str(),
        )
        .build();
    if let Some(preview) = preview {
        let mut preview_elem =
            minidom::Element::builder("preview", waddle_xmpp::xep::NS_WADDLE_PIN_V0).build();
        let author = minidom::Element::builder("author", waddle_xmpp::xep::NS_WADDLE_PIN_V0)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                preview.author_jid.to_string().as_str(),
            )
            .build();
        preview_elem.append_child(author);
        let mut text =
            minidom::Element::builder("text", waddle_xmpp::xep::NS_WADDLE_PIN_V0).build();
        text.append_text_node(&preview.text);
        preview_elem.append_child(text);
        let mut ts = minidom::Element::builder("ts", waddle_xmpp::xep::NS_WADDLE_PIN_V0).build();
        ts.append_text_node(preview.message_timestamp.to_rfc3339());
        preview_elem.append_child(ts);
        pin_event.append_child(preview_elem);
    }
    event.payloads.push(pin_event);
    event
}

async fn fanout_dm_pin_event(
    state: &WebSocketState,
    sender: &jid::BareJid,
    recipients: &[jid::BareJid],
    event: xmpp_parsers::message::Message,
) {
    for resource in state
        .deps
        .protocol
        .connection_registry
        .list_connections()
        .into_iter()
        .filter(|resource| {
            recipients
                .iter()
                .any(|recipient| resource.to_bare() == *recipient)
        })
    {
        let recipient = resource.to_bare();
        if dm_pin_delivery_blocked(state, sender, &recipient).await {
            debug!(
                from = %sender,
                to = %recipient,
                "Suppressing DM pin event because XEP-0191 blocks delivery"
            );
            continue;
        }
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, Stanza::Message(event.clone()));
    }
}

async fn dm_pin_delivery_blocked(
    state: &WebSocketState,
    sender: &jid::BareJid,
    recipient: &jid::BareJid,
) -> bool {
    if sender == recipient {
        return false;
    }
    let sender_jid = jid::Jid::from(sender.clone());
    let recipient_jid = jid::Jid::from(recipient.clone());
    match state
        .deps
        .protocol
        .blocking_storage
        .list_blocked_jid_entries(recipient)
        .await
    {
        Ok(entries) => {
            if waddle_xmpp::protocol::Blocklist::new(entries).contains_jid(&sender_jid) {
                return true;
            }
        }
        Err(error) => {
            warn!(
                from = %sender,
                to = %recipient,
                %error,
                "Failed to check recipient blocklist for DM pin event; failing closed"
            );
            return true;
        }
    }
    match state
        .deps
        .protocol
        .blocking_storage
        .list_blocked_jid_entries(sender)
        .await
    {
        Ok(entries) => waddle_xmpp::protocol::Blocklist::new(entries).contains_jid(&recipient_jid),
        Err(error) => {
            warn!(
                from = %sender,
                to = %recipient,
                %error,
                "Failed to check sender blocklist for DM pin event; failing closed"
            );
            true
        }
    }
}

pub(super) async fn handle_dm_pin_retraction_cascade(
    incoming: &xmpp_parsers::message::Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) {
    if incoming.type_ != xmpp_parsers::message::MessageType::Chat {
        return;
    }
    let Some(waddle_xmpp::xep::RetractionKind::Request(retraction)) =
        waddle_xmpp::xep::extract_retraction_from_message(incoming)
    else {
        return;
    };
    let Some(peer) = incoming.to.as_ref().map(|to| to.to_bare()) else {
        return;
    };
    if peer.domain() != bound_jid.domain() {
        return;
    }
    let sender = bound_jid.to_bare();
    let key = crate::server::routes::websocket::DmPairKey::new(sender.clone(), peer.clone());
    let Some(target) = lookup_dm_pin_target(
        state,
        [&peer, &sender],
        &sender,
        &peer,
        retraction.retracts_id.as_str(),
    )
    .await
    else {
        return;
    };
    if target.archived.from.to_bare() != sender {
        return;
    }
    if !state
        .deps
        .protocol
        .dm_pin_store
        .contains(&key, &target.canonical_stanza_id)
    {
        return;
    }
    if !state
        .deps
        .protocol
        .dm_pin_store
        .unpin(&key, &target.canonical_stanza_id)
    {
        return;
    }
    let event = build_dm_pin_event_message(
        &sender,
        &peer,
        DmPinAction::Unpinned,
        &target.canonical_stanza_id,
        &sender,
        None,
    );
    let mut event = event;
    if let Some(pin_event) = event.payloads.iter_mut().find(|payload| {
        payload.name() == "pin-event" && payload.ns() == waddle_xmpp::xep::NS_WADDLE_PIN_V0
    }) {
        pin_event.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("reason").to_owned(),
            "retracted",
        );
    }
    fanout_dm_pin_event(state, &sender, &[sender.clone(), peer], event).await;
}
