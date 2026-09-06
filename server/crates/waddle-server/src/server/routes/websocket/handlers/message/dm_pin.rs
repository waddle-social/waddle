use std::collections::BTreeMap;

use tracing::{debug, warn};
use waddle_xmpp::{
    ingress::{FrozenStanzaError, IngressEffectIntent},
    protocol::handlers::errors::message_error_reply,
    Stanza,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::ingress::IngressEffectCapture;
use crate::server::routes::interpret::{
    effects::{
        delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        Effect, EffectOutcome, ExternalEffect, PlanEffectDependency, PlanSuppressionPolicy,
        PlannedEffect,
    },
    Deps,
};
use crate::server::routes::websocket::WebSocketState;

pub(super) async fn handle_dm_pin_message(
    incoming: &xmpp_parsers::message::Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
) -> Option<Vec<Stanza>> {
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
                return dm_pin_error_frame(
                    incoming,
                    bound_jid,
                    deps,
                    StanzaError::new(
                        ErrorType::Modify,
                        DefinedCondition::BadRequest,
                        "en",
                        "Malformed DM pin marker.",
                    ),
                )
                .await;
            }
            return None;
        }
    };
    let target = intent.target().to_string();
    let Some(peer) = incoming.to.as_ref().map(|to| to.to_bare()) else {
        return dm_pin_error_frame(
            incoming,
            bound_jid,
            deps,
            StanzaError::new(
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "en",
                "DM pin marker requires a local peer JID.",
            ),
        )
        .await;
    };
    if peer.domain() != bound_jid.domain() {
        return dm_pin_error_frame(
            incoming,
            bound_jid,
            deps,
            xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Auth,
                xmpp_parsers::stanza_error::DefinedCondition::Forbidden,
                "en",
                "DM pins are only supported for local peers.",
            ),
        )
        .await;
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
            .contains(&key, &target.canonical_stanza_id)
        {
            return Some(Vec::new());
        }
        let mutation = DmPinMutation {
            pair: key,
            target_stanza_id: target.canonical_stanza_id.clone(),
            action: waddle_xmpp::ingress::DmPinMutationAction::Unpin,
        };
        if !plan_dm_pin_mutation(deps, &mutation).await {
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
        fanout_dm_pin_event(
            state,
            &sender,
            &[sender.clone(), peer],
            event,
            Some(&mutation),
            deps,
        )
        .await;
        return Some(Vec::new());
    }
    let target = match lookup_dm_pin_target(state, [&peer, &sender], &sender, &peer, &target).await
    {
        Some(found) => found,
        None => {
            return dm_pin_error_frame(
                incoming,
                bound_jid,
                deps,
                xmpp_parsers::stanza_error::StanzaError::new(
                    xmpp_parsers::stanza_error::ErrorType::Cancel,
                    xmpp_parsers::stanza_error::DefinedCondition::ItemNotFound,
                    "en",
                    "Pinned DM target was not found.",
                ),
            )
            .await;
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
    let mutation = DmPinMutation {
        pair: key,
        target_stanza_id: entry.target_stanza_id.clone(),
        action: waddle_xmpp::ingress::DmPinMutationAction::Pin {
            entry: entry.clone(),
        },
    };
    if !plan_dm_pin_mutation(deps, &mutation).await {
        return Some(Vec::new());
    }

    let event = build_dm_pin_event_message(
        &sender,
        &peer,
        DmPinAction::Pinned,
        &entry.target_stanza_id,
        &entry.pinner_jid,
        Some(&entry.preview),
    );
    fanout_dm_pin_event(
        state,
        &sender,
        &[sender.clone(), peer],
        event,
        Some(&mutation),
        deps,
    )
    .await;
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
    mutation: Option<&DmPinMutation>,
    deps: &Deps<'_>,
) {
    let mut deliverable_resources = Vec::new();
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
        deliverable_resources.push(resource.clone());
    }
    let mut accepted_resources = Vec::new();
    for resource in deliverable_resources {
        let mut effect = PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                jid: resource.clone(),
                stanza: Box::new(Stanza::Message(event.clone())),
                kind: PeerDeliveryKind::RegistryFrame,
                call_setup: None,
            },
        )));
        if let Some(mutation) = mutation {
            effect
                .dependencies
                .push(PlanEffectDependency::AfterDmPinMutation {
                    pair: mutation.pair.clone(),
                    target: mutation.target_stanza_id.clone(),
                });
            preserve_retraction_cascade(&mut effect, mutation);
        }
        let outcome = deps.effects.execute(effect, deps).await;
        if matches!(
            outcome,
            EffectOutcome::Delivery(
                crate::server::routes::interpret::FullJidDeliveryOutcome::Delivered
            )
        ) {
            accepted_resources.push(resource);
        }
    }
    capture_dm_pin_routes(deps.ingress_effect_capture.as_ref(), &accepted_resources);
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
    deps: &Deps<'_>,
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
    let mutation = DmPinMutation {
        pair: key,
        target_stanza_id: target.canonical_stanza_id.clone(),
        action: waddle_xmpp::ingress::DmPinMutationAction::RetractionCascadeUnpin,
    };
    if !plan_dm_pin_mutation(deps, &mutation).await {
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
    fanout_dm_pin_event(
        state,
        &sender,
        &[sender.clone(), peer],
        event,
        Some(&mutation),
        deps,
    )
    .await;
}

fn record_dm_pin_mutation(
    ingress_effect_capture: Option<&IngressEffectCapture>,
    pair: &crate::server::routes::websocket::DmPairKey,
    target_stanza_id: StanzaId,
    action: waddle_xmpp::ingress::DmPinMutationAction,
) {
    let Some(capture) = ingress_effect_capture else {
        return;
    };
    capture.record_intent(IngressEffectIntent::DmPinMutation {
        pair: (pair.low_peer.clone(), pair.high_peer.clone()),
        target_stanza_id,
        action,
    });
}

fn capture_dm_pin_routes(
    ingress_effect_capture: Option<&IngressEffectCapture>,
    resources: &[jid::FullJid],
) {
    let Some(capture) = ingress_effect_capture else {
        return;
    };
    let mut fanout_by_recipient: BTreeMap<jid::BareJid, Vec<jid::FullJid>> = BTreeMap::new();
    for resource in resources {
        fanout_by_recipient
            .entry(resource.to_bare())
            .or_default()
            .push(resource.clone());
    }
    for (recipient, mut fanout) in fanout_by_recipient {
        fanout.sort_by_key(ToString::to_string);
        fanout.dedup();
        capture.record_intent(IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            route_identity: capture.next_route_identity(),
        });
    }
}

async fn dm_pin_error_frame(
    incoming: &xmpp_parsers::message::Message,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
    error: StanzaError,
) -> Option<Vec<Stanza>> {
    let frozen_error =
        FrozenStanzaError::from_xmpp(&error).expect("server-built stanza error should freeze");
    let mut stamped = incoming.clone();
    stamped.from = Some(jid::Jid::from(bound_jid.clone()));
    let rejection = super::classify_rejection(&error);
    let reply = Stanza::Message(message_error_reply(&stamped, error));
    if !deps.effects.is_planning() {
        deps.capture_intent(IngressEffectIntent::ErrorReply {
            recipient: bound_jid.clone(),
            error: frozen_error,
        });
    }
    Some(super::reject_message(deps, reply, rejection))
}

#[derive(Clone, Debug)]
pub struct DmPinMutation {
    pub pair: crate::server::routes::websocket::DmPairKey,
    pub target_stanza_id: StanzaId,
    pub action: waddle_xmpp::ingress::DmPinMutationAction,
}

fn preserve_retraction_cascade(effect: &mut PlannedEffect, mutation: &DmPinMutation) {
    if matches!(
        mutation.action,
        waddle_xmpp::ingress::DmPinMutationAction::RetractionCascadeUnpin
    ) {
        effect.tombstone_suppression = PlanSuppressionPolicy::Always;
    }
}

async fn plan_dm_pin_mutation(deps: &Deps<'_>, mutation: &DmPinMutation) -> bool {
    let mut effect = PlannedEffect::new(Effect::External(ExternalEffect::DmPinMutation(
        mutation.clone(),
    )));
    preserve_retraction_cascade(&mut effect, mutation);
    let completed = matches!(
        deps.effects.execute(effect, deps).await,
        EffectOutcome::Completed
    );
    if completed && deps.effects.is_planning() {
        record_dm_pin_mutation(
            deps.ingress_effect_capture.as_ref(),
            &mutation.pair,
            mutation.target_stanza_id.clone(),
            mutation.action.clone(),
        );
    }
    completed
}

pub(crate) async fn execute_dm_pin(mutation: DmPinMutation, deps: &Deps<'_>) -> EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return EffectOutcome::Unavailable;
    };
    let store = &state.deps.protocol.dm_pin_store;
    match &mutation.action {
        waddle_xmpp::ingress::DmPinMutationAction::Pin { entry } => {
            store.apply_pin(mutation.pair.clone(), entry.clone());
        }
        waddle_xmpp::ingress::DmPinMutationAction::Unpin
        | waddle_xmpp::ingress::DmPinMutationAction::RetractionCascadeUnpin => {
            if !store.unpin(&mutation.pair, &mutation.target_stanza_id) {
                return EffectOutcome::Unavailable;
            }
        }
    }
    record_dm_pin_mutation(
        deps.ingress_effect_capture.as_ref(),
        &mutation.pair,
        mutation.target_stanza_id,
        mutation.action,
    );
    EffectOutcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, register_test_connection,
    };
    use waddle_xmpp::mam::ArchivedMessage;
    use waddle_xmpp::xep::build_pinned_message_element;

    #[tokio::test]
    async fn fanout_dm_pin_event_records_direct_routes_per_participant() {
        let state = create_test_websocket_state().await;
        let capture = IngressEffectCapture::new();
        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("alice phone");
        let alice_laptop: jid::FullJid = "alice@example.com/laptop".parse().expect("alice laptop");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("bob phone");
        let (alice_phone_tx, _alice_phone_rx) = tokio::sync::mpsc::channel(4);
        let (alice_laptop_tx, _alice_laptop_rx) = tokio::sync::mpsc::channel(4);
        let (bob_phone_tx, _bob_phone_rx) = tokio::sync::mpsc::channel(4);
        register_test_connection(state.as_ref(), &alice_phone, alice_phone_tx).await;
        register_test_connection(state.as_ref(), &alice_laptop, alice_laptop_tx).await;
        register_test_connection(state.as_ref(), &bob_phone, bob_phone_tx).await;

        let event = build_dm_pin_event_message(
            &"alice@example.com".parse().expect("sender"),
            &"bob@example.com".parse().expect("peer"),
            DmPinAction::Pinned,
            &StanzaId::new(
                "pin-1",
                jid::Jid::from("alice@example.com".parse::<jid::BareJid>().expect("bare")),
            ),
            &"alice@example.com".parse().expect("by"),
            None,
        );

        fanout_dm_pin_event(
            state.as_ref(),
            &"alice@example.com".parse().expect("sender"),
            &[
                "alice@example.com".parse().expect("alice"),
                "bob@example.com".parse().expect("bob"),
            ],
            event,
            None,
            &deps,
        )
        .await;

        let snapshot = capture.snapshot();
        assert!(snapshot.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { recipient, fanout, .. } if *recipient == "alice@example.com".parse::<jid::BareJid>().expect("alice bare") && *fanout == vec![alice_laptop.clone(), alice_phone.clone()])));
        assert!(snapshot.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { recipient, fanout, .. } if *recipient == "bob@example.com".parse::<jid::BareJid>().expect("bob bare") && *fanout == vec![bob_phone.clone()])));
    }

    #[tokio::test]
    async fn fanout_dm_pin_event_ignores_closed_resources_in_route_intent() {
        let state = create_test_websocket_state().await;
        let capture = IngressEffectCapture::new();
        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("alice phone");
        let alice_laptop: jid::FullJid = "alice@example.com/laptop".parse().expect("alice laptop");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("bob phone");
        let (alice_phone_tx, _alice_phone_rx) = tokio::sync::mpsc::channel(4);
        let (alice_laptop_tx, alice_laptop_rx) = tokio::sync::mpsc::channel(4);
        let (bob_phone_tx, _bob_phone_rx) = tokio::sync::mpsc::channel(4);
        drop(alice_laptop_rx);
        register_test_connection(state.as_ref(), &alice_phone, alice_phone_tx).await;
        register_test_connection(state.as_ref(), &alice_laptop, alice_laptop_tx).await;
        register_test_connection(state.as_ref(), &bob_phone, bob_phone_tx).await;

        let event = build_dm_pin_event_message(
            &"alice@example.com".parse().expect("sender"),
            &"bob@example.com".parse().expect("peer"),
            DmPinAction::Pinned,
            &StanzaId::new(
                "pin-1",
                jid::Jid::from("alice@example.com".parse::<jid::BareJid>().expect("bare")),
            ),
            &"alice@example.com".parse().expect("by"),
            None,
        );

        fanout_dm_pin_event(
            state.as_ref(),
            &"alice@example.com".parse().expect("sender"),
            &[
                "alice@example.com".parse().expect("alice"),
                "bob@example.com".parse().expect("bob"),
            ],
            event,
            None,
            &deps,
        )
        .await;

        let snapshot = capture.snapshot();
        assert!(snapshot.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { recipient, fanout, .. } if *recipient == "alice@example.com".parse::<jid::BareJid>().expect("alice bare") && *fanout == vec![alice_phone.clone()])));
        assert!(!snapshot.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { recipient, fanout, .. } if *recipient == "alice@example.com".parse::<jid::BareJid>().expect("alice bare") && *fanout == vec![alice_laptop.clone()])));
    }

    #[tokio::test]
    async fn missing_dm_pin_target_records_error_reply_intent() {
        let state = create_test_websocket_state().await;
        let capture = IngressEffectCapture::new();
        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let mut message = xmpp_parsers::message::Message::new(Some(
            "bob@example.com".parse::<jid::Jid>().expect("peer jid"),
        ));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message.from = Some(jid::Jid::from(sender.clone()));
        message
            .payloads
            .push(build_pinned_message_element(&StanzaId::new(
                "missing-target",
                jid::Jid::from(
                    "alice@example.com"
                        .parse::<jid::BareJid>()
                        .expect("bare jid"),
                ),
            )));

        let frames = handle_dm_pin_message(&message, state.as_ref(), &sender, &deps)
            .await
            .expect("handler should reply");

        assert_eq!(frames.len(), 1);
        let Stanza::Message(reply) = &frames[0] else {
            panic!("expected message reply");
        };
        assert!(waddle_xmpp::parser::stanza_to_string(reply.clone())
            .expect("serialize")
            .contains("item-not-found"));
        let expected_error = FrozenStanzaError::from_xmpp(&StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "en",
            "Pinned DM target was not found.",
        ))
        .expect("server-built stanza error should freeze");
        assert!(capture
            .snapshot()
            .intents
            .contains(&IngressEffectIntent::ErrorReply {
                recipient: sender,
                error: expected_error,
            }));
    }

    #[tokio::test]
    async fn dm_pin_capture_preserves_the_committed_entry() {
        let state = create_test_websocket_state().await;
        let capture = IngressEffectCapture::new();
        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let sender_bare = sender.to_bare();
        let peer: jid::BareJid = "bob@example.com".parse().expect("peer");

        state
            .deps
            .protocol
            .mam_storage
            .store_message(
                &sender_bare,
                &ArchivedMessage {
                    id: "mam-1".to_string(),
                    body: Some("important body".to_string()),
                    message_type: xmpp_parsers::message::MessageType::Chat,
                    stanza_id: Some(StanzaId::new(
                        "target-1",
                        jid::Jid::from(sender_bare.clone()),
                    )),
                    ..ArchivedMessage::for_test(
                        jid::Jid::from(sender.clone()),
                        jid::Jid::from(peer.clone()),
                    )
                },
            )
            .await
            .expect("seed DM archive target");

        let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(peer.clone())));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message.from = Some(jid::Jid::from(sender.clone()));
        message
            .payloads
            .push(build_pinned_message_element(&StanzaId::new(
                "target-1",
                jid::Jid::from(sender_bare.clone()),
            )));

        let frames = handle_dm_pin_message(&message, state.as_ref(), &sender, &deps)
            .await
            .expect("handler should complete");
        assert!(
            frames.is_empty(),
            "successful DM pin stays on the event path"
        );
        assert!(capture.snapshot().intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::DmPinMutation {
                pair,
                action: waddle_xmpp::ingress::DmPinMutationAction::Pin { entry },
                ..
            } if pair == &(sender_bare.clone(), peer.clone())
                && entry.target_stanza_id.id == "target-1"
                && entry.pinner_jid == sender_bare
                && entry.preview.text == "important body"
        )));
    }
    #[tokio::test]
    async fn dm_pin_and_retraction_plan_external_effects_without_store_mutation() {
        use crate::server::routes::interpret::effects::{PlanSink, PlanSuppressionPolicy};
        let state = create_test_websocket_state().await;
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let peer: jid::FullJid = "bob@example.com/web".parse().expect("peer");
        let sender_bare = sender.to_bare();
        let peer_bare = peer.to_bare();
        let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(4);
        let (peer_tx, mut peer_rx) = tokio::sync::mpsc::channel(4);
        register_test_connection(state.as_ref(), &sender, sender_tx).await;
        register_test_connection(state.as_ref(), &peer, peer_tx).await;
        let target = StanzaId::new("plan-pin-target", jid::Jid::from(sender_bare.clone()));
        state
            .deps
            .protocol
            .mam_storage
            .store_message(
                &sender_bare,
                &ArchivedMessage {
                    id: "plan-pin-archive".to_owned(),
                    body: Some("important body".to_owned()),
                    message_type: xmpp_parsers::message::MessageType::Chat,
                    stanza_id: Some(target.clone()),
                    ..ArchivedMessage::for_test(sender.clone().into(), peer_bare.clone().into())
                },
            )
            .await
            .expect("seed archive");
        let mut message = xmpp_parsers::message::Message::new(Some(peer_bare.clone().into()));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message.payloads.push(build_pinned_message_element(&target));
        let sink = PlanSink::new();
        let capture = IngressEffectCapture::new();
        let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        );
        deps.effects = &sink;
        deps.ingress_effect_capture = Some(capture.clone());
        handle_dm_pin_message(&message, state.as_ref(), &sender, &deps)
            .await
            .expect("pin handled");
        let pair = crate::server::routes::websocket::DmPairKey::new(sender_bare, peer_bare);
        assert!(state.deps.protocol.dm_pin_store.list(&pair).is_empty());
        assert!(sender_rx.try_recv().is_err());
        assert!(peer_rx.try_recv().is_err());
        assert!(capture.snapshot().intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::DmPinMutation {
                action: waddle_xmpp::ingress::DmPinMutationAction::Pin { .. },
                ..
            }
        )));
        let effects = sink.snapshot();
        let mutation = effects
            .iter()
            .find_map(|effect| match &effect.effect {
                Effect::External(ExternalEffect::DmPinMutation(mutation)) => {
                    assert_eq!(effect.suppression, PlanSuppressionPolicy::Always);
                    Some(mutation.clone())
                }
                _ => None,
            })
            .expect("planned pin mutation");
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(
                    effect.effect,
                    Effect::External(ExternalEffect::Delivery(
                        ExternalDeliveryEffect::RouteToPeer { .. }
                    ))
                ))
                .count(),
            2
        );
        for effect in &effects {
            assert_eq!(
                effect.tombstone_suppression,
                PlanSuppressionPolicy::TombstoneSwallowed
            );
            if matches!(effect.effect, Effect::External(ExternalEffect::Delivery(_))) {
                assert!(effect
                    .dependencies
                    .contains(&PlanEffectDependency::AfterDmPinMutation {
                        pair: pair.clone(),
                        target: target.clone(),
                    }));
            }
        }
        execute_dm_pin(mutation, &deps).await;
        assert!(state.deps.protocol.dm_pin_store.contains(&pair, &target));
        sink.take();
        message.payloads.clear();
        message
            .payloads
            .push(waddle_xmpp::xep::build_retract_element("plan-pin-target"));
        handle_dm_pin_retraction_cascade(&message, state.as_ref(), &sender, &deps).await;
        assert!(state.deps.protocol.dm_pin_store.contains(&pair, &target));
        assert!(sender_rx.try_recv().is_err());
        assert!(peer_rx.try_recv().is_err());
        let effects = sink.snapshot();
        for effect in &effects {
            assert_eq!(
                effect.tombstone_suppression,
                PlanSuppressionPolicy::Always,
                "retraction cleanup survives the tombstone it creates"
            );
            if matches!(effect.effect, Effect::External(ExternalEffect::Delivery(_))) {
                assert!(effect
                    .dependencies
                    .contains(&PlanEffectDependency::AfterDmPinMutation {
                        pair: pair.clone(),
                        target: target.clone(),
                    }));
            }
        }
        assert!(effects.iter().any(|effect| matches!(
            &effect.effect,
            Effect::External(ExternalEffect::DmPinMutation(DmPinMutation {
                action: waddle_xmpp::ingress::DmPinMutationAction::RetractionCascadeUnpin,
                ..
            }))
        )));
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(
                    effect.effect,
                    Effect::External(ExternalEffect::Delivery(
                        ExternalDeliveryEffect::RouteToPeer { .. }
                    ))
                ))
                .count(),
            2
        );
    }
}
