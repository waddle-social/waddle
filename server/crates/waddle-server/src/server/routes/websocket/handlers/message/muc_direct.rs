use waddle_xmpp::{
    ingress::{IngressEffectIntent, MucInviteLedgerAction, MucInviteLedgerMutation},
    muc::room_registry_actor::GetRoom,
    protocol::handlers::errors::message_error_reply,
    Stanza,
};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::ingress::IngressEffectCapture;
use crate::server::routes::interpret::{
    effects::{Effect, ExternalEffect, PlannedEffect},
    Deps,
};
use crate::server::routes::websocket::WebSocketState;
use tracing::warn;

pub(super) async fn handle_muc_direct_message(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
) -> Option<Vec<Stanza>> {
    if let Some(frames) = handle_muc_private_message(incoming, state, bound_jid, deps).await {
        return Some(frames);
    }
    handle_muc_mediated_decline(incoming, state, bound_jid, deps).await
}

async fn handle_muc_private_message(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
) -> Option<Vec<Stanza>> {
    let ingress_effect_capture = deps.ingress_effect_capture.as_ref();
    let target_occupant_jid = incoming.to.as_ref()?.clone().try_into_full().ok()?;
    let room_jid = target_occupant_jid.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }
    if incoming.type_ == MessageType::Groupchat {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "Groupchat messages must be addressed to the room bare JID.",
        )]);
    }
    if !matches!(incoming.type_, MessageType::Chat | MessageType::Normal) {
        return None;
    }
    let target_nick = target_occupant_jid.resource().to_string();

    let Some(room_actor) = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
        .ok()
        .flatten()
    else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested room not found.",
        )]);
    };
    let Ok(snapshot) = room_actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    };
    if deps.effects.is_planning() {
        use crate::server::routes::interpret::effects::{
            room::RoomFenceRequirement, RoomExecutionPath,
        };
        deps.effects.set_room_execution(RoomExecutionPath::Local {
            room: room_jid.clone(),
            fence: snapshot
                .claim_fence
                .clone()
                .map(RoomFenceRequirement::Guarded)
                .unwrap_or(RoomFenceRequirement::Unfenced),
            snapshot_generation: snapshot.admission_revision,
        });
    }
    let Some(sender_occupant) = snapshot.room.find_occupant_by_real_jid(bound_jid) else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "Only room occupants may send private messages.",
        )]);
    };
    let sender_nick = sender_occupant.nick.clone();
    // XEP-0045 `muc#roomconfig_allowpm` (#1257): honor the room's PM
    // policy against the sender's current role.
    if !snapshot.room.config.allow_pm.permits(sender_occupant.role) {
        // RFC 6120 §8.3.3.5: <forbidden/> is associated with type='auth'
        // (matches every §7/§8 example in XEP-0045 and this repo's
        // groupchat forbidden_error helper).
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "Private messages are not allowed for your role in this room.",
        )]);
    }
    let Some(target_occupant) = snapshot.room.get_occupant(&target_nick) else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested occupant not found.",
        )]);
    };
    let recipient_bare = target_occupant.real_jid.to_bare();
    let recipient_sessions = snapshot.room.get_occupant_sessions(&target_nick);

    let from_room_jid = match room_jid.clone().with_resource_str(&sender_nick) {
        Ok(jid) => jid,
        Err(_) => {
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    };
    // XEP-0421 §Business Rules: "The <occupant-id/> element MUST be
    // attached to every message ... sent by a MUC" — private messages
    // included. Derive the sender's stable occupant-id the same way
    // the groupchat canonicalize handler does (#1268).
    let sender_occupant_id = waddle_xmpp::xep::xep0421::generate_occupant_id(
        &bound_jid.to_bare(),
        &room_jid,
        &state.deps.occupant_id_secret,
    );

    // Canonical relayed form (XEP-0045 §7.5): from the sender's occupant
    // JID, client MUC-service payloads stripped, empty muc#user marker +
    // server occupant-id stamped (#1251/#1268).
    let mut relayed = incoming.clone();
    relayed.from = Some(jid::Jid::from(from_room_jid.clone()));
    canonicalize_muc_private_payloads(&mut relayed, &sender_occupant_id);

    let sender_bare = bound_jid.to_bare();
    let mut sent_form = incoming.clone();
    sent_form.from = Some(jid::Jid::from(bound_jid.clone()));
    sent_form.to = Some(jid::Jid::from(target_occupant_jid.clone()));
    canonicalize_muc_private_payloads(&mut sent_form, &sender_occupant_id);

    // #1257: archive + carbons through the same interpreter arms the 1:1
    // pipeline uses — and BEFORE the wire copies go out (review P1/P2 on
    // PR #1277): stanza-ids are stamped ONLY on copies that are actually
    // archived (body-less chat-state PMs get none — an id with no MAM
    // row behind it is a lie), and interpreting the archive first lets
    // an origin-id dedupe's ArchiveIdRewrite land on the wire copy, so
    // live/MAM id parity holds under retries. Archive ownership remains
    // the user's bare JID. The peer endpoint retains the full occupant JID
    // required for exact `with=room/nick` matching, while the owner endpoint
    // preserves the stanza the user actually sent or received (§7.5).
    let should_archive = !incoming.bodies.is_empty();
    let mut events: Vec<waddle_xmpp::protocol::OutboundEvent> = Vec::new();
    if should_archive {
        let recipient_sid = waddle_xmpp_core::xep0359::StanzaId::new(
            uuid::Uuid::new_v4().to_string(),
            jid::Jid::from(recipient_bare.clone()),
        );
        waddle_xmpp_core::xep0359::add_stanza_id(&mut relayed, &recipient_sid);
        // Self-PM (own nick): only ONE archive row is written (the
        // recipient/owner archive below), so the sent-carbon copy must
        // carry that single backed id — a fresh sender-side id would
        // reference a row that never exists (Greptile P1 on PR #1277).
        let sender_sid = if recipient_bare == sender_bare {
            recipient_sid.clone()
        } else {
            waddle_xmpp_core::xep0359::StanzaId::new(
                uuid::Uuid::new_v4().to_string(),
                jid::Jid::from(sender_bare.clone()),
            )
        };
        waddle_xmpp_core::xep0359::add_stanza_id(&mut sent_form, &sender_sid);

        let occupant_from = jid::Jid::from(from_room_jid.clone());
        let occupant_to = jid::Jid::from(target_occupant_jid.clone());
        let mut recipient_archive = relayed.clone();
        recipient_archive.to = Some(jid::Jid::from(recipient_bare.clone()));
        events.push(waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
            archive_jid: recipient_bare.clone(),
            from: occupant_from.clone(),
            to: jid::Jid::from(recipient_bare.clone()),
            message: Box::new(recipient_archive),
        });
        // A self-PM (own nick) would otherwise archive twice into the
        // same owner's archive.
        if recipient_bare != sender_bare {
            events.push(waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
                archive_jid: sender_bare.clone(),
                from: jid::Jid::from(sender_bare.clone()),
                to: occupant_to,
                message: Box::new(sent_form.clone()),
            });
        }
    }
    // Bodyless PMs with carbon suppression emit no interpreter events, but
    // their committed envelope still needs the canonical sender-side payload.
    deps.effects.observe_message(&sent_form);
    // XEP-0280 eligibility (review P2 on PR #1277): honor §6.1
    // `<private/>` and XEP-0334 `<no-copy/>` suppression — the shared
    // `should_copy_message` rule the 1:1 CarbonsMessageHandler applies —
    // since this path emits SendCarbons directly. The exclusion set is
    // the original delivery set per §6.3: every session of the target
    // nick PLUS the originating resource, so a self-PM (own nick) never
    // double-delivers to a sibling resource that already got the live
    // relayed copy. Ordered AFTER the archive events so the interpreter
    // applies any ArchiveIdRewrite to the carbon inner message too.
    // Deliberate SHOULD-level deviation: the copy goes to ALL other
    // carbon-enabled resources, not only Multi-Session-Nick clients in
    // this room — XEP-0280's note allows this (clients not joined
    // "SHOULD either ignore such carbon copies, or provide a way for
    // the user to join the MUC before answering").
    if waddle_xmpp_core::carbons::should_copy_message(incoming) {
        let mut exclude = recipient_sessions.clone();
        if !exclude.contains(bound_jid) {
            exclude.push(bound_jid.clone());
        }
        events.push(waddle_xmpp::protocol::OutboundEvent::SendCarbons {
            owner: sender_bare,
            message: Box::new(sent_form),
            kind: waddle_xmpp::protocol::CarbonKind::Sent,
            exclude,
        });
    }
    let nested = crate::server::routes::interpret::interpret(events, deps).await;
    if !nested.archive_id_rewrites.is_empty() {
        crate::server::routes::interpret::rewrite_message_archive_ids(
            &mut relayed,
            &nested.archive_id_rewrites,
        );
    }

    // #1257: reliable delivery. Replace the previous fire-and-forget
    // live-only `try_send_to` (which silently lost the PM for a
    // recipient mid-SM-resume, detached, backpressured, or on another
    // node) with the same actor delivery path 1:1 direct frames use:
    // live channel first, XEP-0198 detached replay buffer as fallback,
    // clustered remote-resource relay for occupant sessions whose
    // socket lives on another node.
    let mut any_session_handled = false;
    let mut definitive_recipients = Vec::new();
    for recipient in &recipient_sessions {
        let mut routed = relayed.clone();
        routed.to = Some(jid::Jid::from(recipient.clone()));
        let stanza = Stanza::Message(routed);
        let outcome = deliver_pm_to_session(deps, recipient, &stanza).await;
        let capture = pm_delivery_capture(outcome);
        if capture.any_session_handled {
            any_session_handled = true;
        }
        if capture.record_definitive_route {
            definitive_recipients.push(recipient.clone());
        } else if !capture.any_session_handled {
            warn!(
                room = %room_jid,
                recipient = %recipient,
                outcome = ?outcome,
                "MUC private message: session delivery failed"
            );
        }
    }
    capture_muc_private_routes(
        ingress_effect_capture,
        &from_room_jid,
        definitive_recipients,
    );
    // XEP-0045 §7.5: the service is responsible for delivering the PM.
    // An ARCHIVED PM that reached no live/detached session is still
    // durable — the occupant sees it via MAM — so only an unarchivable
    // (body-less) PM that reached nobody bounces to the sender; a
    // wait-class error after the archive committed would misreport a
    // message the recipient will in fact see.
    if !any_session_handled {
        if should_archive {
            warn!(
                room = %room_jid,
                target_nick = %target_nick,
                "MUC private message: no session reachable; archived copy remains durable"
            );
        } else {
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Wait,
                DefinedCondition::RecipientUnavailable,
                "The occupant could not be reached; please retry.",
            )]);
        }
    }

    debug_assert!(
        nested.frames.is_empty(),
        "archive and carbon events return no frames"
    );
    Some(Vec::new())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PmDeliveryCapture {
    any_session_handled: bool,
    record_definitive_route: bool,
}

fn pm_delivery_capture(
    outcome: crate::server::routes::interpret::FullJidDeliveryOutcome,
) -> PmDeliveryCapture {
    match outcome {
        crate::server::routes::interpret::FullJidDeliveryOutcome::Delivered
        | crate::server::routes::interpret::FullJidDeliveryOutcome::QueuedDetached => {
            PmDeliveryCapture {
                any_session_handled: true,
                record_definitive_route: true,
            }
        }
        #[cfg(feature = "clustering")]
        crate::server::routes::interpret::FullJidDeliveryOutcome::MaybeCommitted => {
            PmDeliveryCapture {
                any_session_handled: true,
                record_definitive_route: false,
            }
        }
        crate::server::routes::interpret::FullJidDeliveryOutcome::Unavailable
        | crate::server::routes::interpret::FullJidDeliveryOutcome::Dropped => PmDeliveryCapture {
            any_session_handled: false,
            record_definitive_route: false,
        },
    }
}

/// Deliver one MUC private-message wire copy to one occupant session
/// (#1257): clustered remote-resource relay first (the session's socket
/// may live on another node), then the actor path with its XEP-0198
/// detached-buffer fallback — the same reliability envelope 1:1 direct
/// frames get.
async fn deliver_pm_to_session(
    deps: &Deps<'_>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> crate::server::routes::interpret::FullJidDeliveryOutcome {
    use crate::server::routes::interpret::effects::{
        delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        EffectOutcome,
    };
    let mut effect = PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
        ExternalDeliveryEffect::RouteToPeer {
            jid: target.clone(),
            stanza: Box::new(stanza.clone()),
            kind: PeerDeliveryKind::DirectFrame,
            call_setup: None,
        },
    )));
    if let Stanza::Message(message) = stanza {
        effect.dependencies.extend(
            waddle_xmpp::xep::extract_stanza_ids(message)
                .into_iter()
                .map(|minted| {
                    crate::server::routes::interpret::effects::PlanEffectDependency::AfterArchive {
                        archive: minted.by.to_bare(),
                        minted,
                    }
                }),
        );
    }
    let EffectOutcome::Delivery(outcome) = deps.effects.execute(effect, deps).await else {
        unreachable!("peer routing returns a delivery outcome");
    };
    outcome
}

/// XEP-0045 §7.8.2 mediated decline, hardened per #1264:
///
/// - the decline is only forwarded when the outstanding-invite ledger
///   holds a row for `(room, decliner)` — without that check any
///   authenticated user could make the room deliver a "declined your
///   invitation" message to an arbitrary user;
/// - with several inviters outstanding, the decline's `to` attribute
///   selects WHICH invitation is declined, but the recipient is always
///   a ledger-recorded inviter — never an arbitrary client-supplied
///   target;
/// - the row is claimed atomically BEFORE delivery (concurrent
///   declines from two devices forward exactly one) and re-recorded if
///   neither a live socket nor the durable queue accepted the decline;
/// - delivery is durable: an offline inviter gets a pending-delivery
///   row instead of a silent drop.
///
/// The room actor is deliberately not consulted: the ledger outlives
/// room-actor dormancy eviction, so a legitimate decline still reaches
/// the inviter after the room actor was evicted.
async fn handle_muc_mediated_decline(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
) -> Option<Vec<Stanza>> {
    let ingress_effect_capture = deps.ingress_effect_capture.as_ref();
    if incoming.type_ != MessageType::Normal {
        return None;
    }
    let room_jid = incoming.to.as_ref()?.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }
    let inbound_decline = mediated_decline(incoming)?;
    let decliner = bound_jid.to_bare();
    let db_actor = state.deps.app_state.db_pool.global_actor().clone();
    let outstanding = match crate::server::routes::websocket::muc_invites::list_invites(
        db_actor.clone(),
        &room_jid,
        &decliner,
    )
    .await
    {
        Ok(outstanding) => outstanding,
        Err(error) => {
            tracing::warn!(
                room = %room_jid,
                decliner = %decliner,
                error = %error,
                "Failed to look up outstanding invites for mediated decline"
            );
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    };
    if outstanding.is_empty() {
        // #1264: no outstanding invitation — refuse instead of
        // relaying a fabricated decline.
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "You have no outstanding invitation to this room.",
        )]);
    }
    // Select which invitation this decline answers. The client's `to`
    // must name one of the recorded inviters; with exactly one
    // outstanding invitation a missing/mismatched `to` is forgiven
    // (there is nothing to disambiguate).
    let declined_to = inbound_decline
        .attr("to")
        .and_then(|to| to.parse::<jid::Jid>().ok())
        .map(|jid| jid.to_bare());
    let invite = match declined_to
        .as_ref()
        .and_then(|to| outstanding.iter().find(|invite| invite.inviter == *to))
    {
        Some(invite) => invite.clone(),
        None if outstanding.len() == 1 => outstanding[0].clone(),
        None => {
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "Several invitations are outstanding; the decline must name its inviter.",
            )]);
        }
    };

    // Claim the row atomically FIRST: of N concurrent declines for the
    // same invitation, exactly one wins the delete and forwards.
    let claim = PlannedEffect::new(Effect::External(ExternalEffect::InviteLedger(
        super::muc_invite::InviteLedgerMutation::Claim {
            invite: invite.clone(),
        },
    )));
    let crate::server::routes::interpret::effects::EffectOutcome::InviteLedger(claimed) =
        deps.effects.execute(claim, deps).await
    else {
        unreachable!("invite ledger effect returns its typed outcome");
    };
    match claimed {
        Ok(super::muc_invite::InviteLedgerOutcome::Claimed(true)) => {}
        Ok(super::muc_invite::InviteLedgerOutcome::Claimed(false)) => return Some(Vec::new()),
        Ok(super::muc_invite::InviteLedgerOutcome::Recorded(_)) => {
            unreachable!("claim effect returns claimed")
        }
        Err(error) => {
            tracing::warn!(
                room = %room_jid,
                decliner = %decliner,
                error = %error,
                "Failed to claim outstanding invite for mediated decline"
            );
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    }

    let x = build_mediated_decline_payload(bound_jid, inbound_decline);
    let mut mediated = Message::new(Some(jid::Jid::from(invite.inviter.clone())));
    mediated.id = incoming.id.clone();
    mediated.from = Some(jid::Jid::from(room_jid.clone()));
    mediated.type_ = MessageType::Normal;
    mediated.payloads.push(x);
    let delivery_sink = crate::server::routes::interpret::effects::ScopedInviteSink {
        inner: deps.effects,
        invite: invite.clone(),
        failure: Some(
            crate::server::routes::interpret::effects::invite::InviteDeliveryFailure::RestoreLedger(
                invite.clone(),
            ),
        ),
    };
    let mut delivery_deps = deps.clone();
    delivery_deps.effects = &delivery_sink;
    if let Err(error) = super::muc_invite::deliver_muc_user_message(
        state,
        &invite.inviter,
        mediated,
        &delivery_deps,
    )
    .await
    {
        // The executed delivery effect restored the claimed ledger row;
        // tell the decliner that the delivery failed so they can retry.
        tracing::warn!(
            room = %room_jid,
            decliner = %decliner,
            inviter = %invite.inviter,
            error = %error,
            "Mediated decline could not be delivered or queued"
        );
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    }

    if let Some(capture) = ingress_effect_capture {
        capture.record_intent(IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: invite.room.clone(),
                invitee: invite.invitee.clone(),
                inviter: invite.inviter.clone(),
                action: MucInviteLedgerAction::Claimed,
                recorded_at: None,
            },
        });
    }

    Some(Vec::new())
}

fn capture_muc_private_routes(
    ingress_effect_capture: Option<&IngressEffectCapture>,
    sender: &jid::FullJid,
    recipients: Vec<jid::FullJid>,
) {
    let Some(capture) = ingress_effect_capture else {
        return;
    };
    let mut recipients = recipients;
    recipients.sort_by_key(ToString::to_string);
    recipients.dedup();
    for recipient in recipients {
        capture.record_intent(IngressEffectIntent::RouteOccupantPm {
            recipient,
            sender: sender.clone(),
        });
    }
}

fn mediated_decline(message: &Message) -> Option<&minidom::Element> {
    message
        .payloads
        .iter()
        .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
        .and_then(|x| x.get_child("decline", waddle_xmpp::muc::presence::NS_MUC_USER))
}

fn build_mediated_decline_payload(
    decliner: &jid::FullJid,
    inbound_decline: &minidom::Element,
) -> minidom::Element {
    let mut decline = minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            decliner.to_bare().to_string(),
        );
    if let Some(reason) =
        inbound_decline.get_child("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
    {
        decline = decline.append(reason.clone());
    }
    minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(decline.build())
        .build()
}

fn canonicalize_muc_private_payloads(
    message: &mut Message,
    sender_occupant_id: &waddle_xmpp::xep::xep0421::OccupantId,
) {
    message.payloads.retain(|payload| {
        // XEP-0313 §Security "MUC message spoofing" + XEP-0045
        // anti-spoofing: strip every client-supplied payload in a MUC
        // *service* namespace (muc / muc#user / muc#admin / muc#owner)
        // so an occupant cannot forge affiliation/role/status/invite
        // signalling on a PM that the server then relays from
        // `room/nick`. Namespace-only, sharing the exact set with the
        // groupchat canonicalizer (#1251, #1268).
        if waddle_xmpp::muc::is_muc_service_namespace(payload.ns().as_str())
            || payload.is("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
        {
            return false;
        }
        // XEP-0359: stanza-ids are assigned by servers/rooms, never by
        // senders. Strip every client-supplied stanza-id from a MUC private
        // message so a client cannot inject a room-spoofing or otherwise
        // misleading identifier — the server is the canonical source.
        if payload.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID) {
            return false;
        }
        true
    });
    message
        .payloads
        .push(minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER).build());
    // XEP-0421: re-stamp the server-derived occupant-id after stripping
    // any client-supplied one (#1268).
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(message, sender_occupant_id);
}

fn message_error_frame(
    incoming: &Message,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
    error_type: ErrorType,
    condition: DefinedCondition,
    text: &'static str,
) -> Stanza {
    let mut stamped = incoming.clone();
    stamped.from = Some(jid::Jid::from(bound_jid.clone()));
    let error = StanzaError::new(error_type, condition, "en", text);
    if let Some(capture) = &deps.ingress_effect_capture {
        capture.record_intent(IngressEffectIntent::ErrorReply {
            recipient: bound_jid.clone(),
            error: waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error)
                .expect("server-built stanza error should freeze"),
        });
    }
    let rejection = super::classify_rejection(&error);
    let reply = Stanza::Message(message_error_reply(&stamped, error));
    super::reject_message(deps, reply.clone(), rejection);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::IngressEffectCapture;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, register_test_connection,
    };
    use kameo::actor::ActorRef;
    use waddle_xmpp::muc::room_actor::Join;
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::xep::xep0421::{
        extract_occupant_id_from_message, generate_occupant_id, OccupantId, OccupantIdSecret,
        NS_OCCUPANT_ID, OCCUPANT_ID_SECRET_MIN_BYTES,
    };
    use waddle_xmpp::{Affiliation, Role};

    fn secret() -> OccupantIdSecret {
        OccupantIdSecret::new(vec![3u8; OCCUPANT_ID_SECRET_MIN_BYTES]).expect("valid secret")
    }

    fn pm() -> Message {
        let mut m = Message::new(Some(
            "room@muc.example.com/bob".parse::<jid::Jid>().expect("jid"),
        ));
        m.type_ = MessageType::Chat;
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), "psst".to_string());
        m
    }

    async fn create_test_room(
        state: &WebSocketState,
        room_jid: jid::BareJid,
    ) -> ActorRef<waddle_xmpp::muc::room_actor::RoomActor> {
        state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid,
                waddle_id: "w-muc-direct".to_string(),
                channel_id: "c-muc-direct".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room")
    }

    /// XEP-0421 Business Rules (#1268): MUC private messages MUST carry
    /// the server-derived occupant-id — stripping the client's forgery
    /// and re-stamping the canonical value.
    #[test]
    fn xep0421_pm_carries_server_stamped_occupant_id() {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let sender_bare: jid::BareJid = "alice@example.com".parse().expect("sender");
        let secret = secret();
        let server_id = generate_occupant_id(&sender_bare, &room, &secret);

        let mut msg = pm();
        // Client tries to spoof someone else's occupant-id.
        msg.payloads
            .push(waddle_xmpp::xep::xep0421::build_occupant_id_element(
                &OccupantId::new("forged-id"),
            ));

        canonicalize_muc_private_payloads(&mut msg, &server_id);

        let stamped = extract_occupant_id_from_message(&msg).expect("occupant-id stamped on PM");
        assert_eq!(stamped, server_id);
        assert_ne!(stamped, OccupantId::new("forged-id"));
        // Exactly one occupant-id element.
        let count = msg
            .payloads
            .iter()
            .filter(|p| p.is("occupant-id", NS_OCCUPANT_ID))
            .count();
        assert_eq!(count, 1);
    }

    /// XEP-0045 §7.5: the PM keeps exactly one empty muc#user `<x/>`
    /// marker (client-supplied ones are stripped) alongside the
    /// occupant-id.
    #[test]
    fn xep0421_pm_keeps_single_empty_muc_user_marker() {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let sender_bare: jid::BareJid = "alice@example.com".parse().expect("sender");
        let secret = secret();
        let server_id = generate_occupant_id(&sender_bare, &room, &secret);

        let mut msg = pm();
        // Forged muc#user x with an item claiming an affiliation.
        msg.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("item", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .attr(
                            minidom::rxml::xml_ncname!("affiliation").to_owned(),
                            "owner",
                        )
                        .build(),
                )
                .build(),
        );

        canonicalize_muc_private_payloads(&mut msg, &server_id);

        let markers: Vec<_> = msg
            .payloads
            .iter()
            .filter(|p| p.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
            .collect();
        assert_eq!(markers.len(), 1, "exactly one muc#user marker");
        assert_eq!(
            markers[0].children().count(),
            0,
            "the PM marker is empty — forged items must not survive"
        );
    }

    /// XEP-0313 §Security / XEP-0045 anti-spoofing (#1251): a PM must
    /// not launder client-supplied payloads in ANY MUC service
    /// namespace (muc / muc#admin / muc#owner), not just muc#user —
    /// including non-`<x>` element names.
    #[test]
    fn xep0045_pm_strips_all_muc_service_namespaces() {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let sender_bare: jid::BareJid = "alice@example.com".parse().expect("sender");
        let secret = secret();
        let server_id = generate_occupant_id(&sender_bare, &room, &secret);

        let mut msg = pm();
        // A non-`<x>` element in muc#user (status code), plus payloads
        // in muc / muc#admin / muc#owner.
        msg.payloads.push(
            minidom::Element::builder("status", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(minidom::rxml::xml_ncname!("code").to_owned(), "110")
                .build(),
        );
        msg.payloads
            .push(minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC).build());
        msg.payloads
            .push(minidom::Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN).build());
        msg.payloads
            .push(minidom::Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build());

        canonicalize_muc_private_payloads(&mut msg, &server_id);

        // Only the single server-authored empty muc#user marker remains
        // in MUC service namespaces; nothing else.
        for ns in [
            waddle_xmpp::muc::presence::NS_MUC,
            waddle_xmpp::muc::NS_MUC_ADMIN,
            waddle_xmpp::muc::NS_MUC_OWNER,
        ] {
            assert!(
                !msg.payloads.iter().any(|p| p.ns() == ns),
                "client payloads in `{ns}` must be stripped from a MUC PM"
            );
        }
        let muc_user: Vec<_> = msg
            .payloads
            .iter()
            .filter(|p| p.ns() == waddle_xmpp::muc::presence::NS_MUC_USER)
            .collect();
        assert_eq!(
            muc_user.len(),
            1,
            "only the server-authored empty muc#user marker survives"
        );
        assert_eq!(muc_user[0].name(), "x");
        assert_eq!(muc_user[0].children().count(), 0);
    }

    #[test]
    fn capture_muc_private_routes_records_each_recipient_once() {
        let capture = IngressEffectCapture::new();
        let sender: jid::FullJid = "room@muc.example.com/alice".parse().expect("sender");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("bob phone");
        let bob_laptop: jid::FullJid = "bob@example.com/laptop".parse().expect("bob laptop");

        capture_muc_private_routes(
            Some(&capture),
            &sender,
            vec![bob_phone.clone(), bob_laptop.clone(), bob_phone.clone()],
        );

        let snapshot = capture.snapshot();
        assert!(snapshot
            .intents
            .contains(&IngressEffectIntent::RouteOccupantPm {
                recipient: bob_laptop,
                sender: sender.clone(),
            }));
        assert!(snapshot
            .intents
            .contains(&IngressEffectIntent::RouteOccupantPm {
                recipient: bob_phone.clone(),
                sender,
            }));
        assert_eq!(
            snapshot
                .intents
                .iter()
                .filter(|intent| matches!(
                    intent,
                    IngressEffectIntent::RouteOccupantPm { recipient, .. }
                        if recipient == &bob_phone
                ))
                .count(),
            1
        );
    }

    #[cfg(feature = "clustering")]
    #[test]
    fn maybe_committed_pm_delivery_is_not_a_definitive_route() {
        let capture = pm_delivery_capture(
            crate::server::routes::interpret::FullJidDeliveryOutcome::MaybeCommitted,
        );

        assert!(capture.any_session_handled);
        assert!(!capture.record_definitive_route);
    }

    #[tokio::test]
    async fn muc_private_rejection_records_error_reply_intent() {
        let state = create_test_websocket_state().await;
        let room: jid::BareJid = "pm@muc.example.com".parse().expect("room");
        let _room_actor = create_test_room(state.as_ref(), room.clone()).await;
        let sender: jid::FullJid = "mallory@example.com/web".parse().expect("sender");
        let capture = IngressEffectCapture::new();
        let mut incoming = Message::new(Some(
            "pm@muc.example.com/bob"
                .parse::<jid::Jid>()
                .expect("target jid"),
        ));
        incoming.type_ = MessageType::Chat;
        incoming.from = Some(jid::Jid::from(sender.clone()));

        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let frames = handle_muc_direct_message(&incoming, state.as_ref(), &sender, &deps)
            .await
            .expect("handled");

        assert_eq!(frames.len(), 1);
        let expected_error = waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "en",
            "Only room occupants may send private messages.",
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
    async fn muc_private_unreachable_bodyless_pm_does_not_record_route_intent() {
        let state = create_test_websocket_state().await;
        let room: jid::BareJid = "pm-capture@muc.example.com".parse().expect("room");
        let room_actor = create_test_room(state.as_ref(), room.clone()).await;
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let recipient: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
        room_actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: sender.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join alice");
        room_actor
            .ask(Join {
                nick: "bob".to_string(),
                real_jid: recipient.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join bob");

        let (recipient_tx, recipient_rx) = tokio::sync::mpsc::channel(8);
        let _owner = register_test_connection(state.as_ref(), &recipient, recipient_tx).await;
        drop(recipient_rx);

        let capture = IngressEffectCapture::new();
        let mut incoming = Message::new(Some(
            format!("{room}/bob")
                .parse::<jid::Jid>()
                .expect("target occupant jid"),
        ));
        incoming.from = Some(jid::Jid::from(sender.clone()));
        incoming.type_ = MessageType::Chat;

        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let frames = handle_muc_direct_message(&incoming, state.as_ref(), &sender, &deps)
            .await
            .expect("handled");

        assert_eq!(frames.len(), 1, "bodyless unreachable PM returns one error");
        assert!(
            !capture
                .snapshot()
                .intents
                .iter()
                .any(|intent| { matches!(intent, IngressEffectIntent::RouteOccupantPm { .. }) }),
            "failed PM delivery must not record RouteOccupantPm"
        );
    }
    #[tokio::test]
    async fn plan_muc_private_message_archives_twice_without_delivering() {
        use crate::server::routes::interpret::effects::{
            delivery::ExternalDeliveryEffect, direct::DurableDirectEffect, DurableEffect, PlanSink,
        };
        let state = create_test_websocket_state().await;
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let actor = create_test_room(state.as_ref(), room).await;
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let recipient: jid::FullJid = "bob@example.com/web".parse().expect("recipient");
        for (nick, jid) in [("alice", &sender), ("bob", &recipient)] {
            actor
                .ask(Join {
                    nick: nick.to_owned(),
                    real_jid: jid.clone(),
                    role: Role::Participant,
                    affiliation: Affiliation::Member,
                })
                .await
                .expect("join");
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let _owner = register_test_connection(state.as_ref(), &recipient, tx).await;
        let sink = PlanSink::new();
        let base = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        );
        let deps = crate::server::routes::interpret::build_plan_deps(&base, &sink);
        let frames = handle_muc_direct_message(&pm(), state.as_ref(), &sender, &deps)
            .await
            .expect("handled");
        assert!(frames.is_empty());
        assert!(rx.try_recv().is_err(), "planning must not deliver");
        let effects = sink.snapshot();
        for effect in &effects {
            if let Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                message,
                ..
            })) = &effect.effect
            {
                assert!(
                    deps.mam_storage
                        .expect("MAM storage")
                        .get_message(&message.id)
                        .await
                        .expect("archive read")
                        .is_none(),
                    "planning must not write archive rows"
                );
            }
        }
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(
                    effect.effect,
                    Effect::Durable(DurableEffect::Direct(
                        DurableDirectEffect::ArchiveDirect { .. }
                    ))
                ))
                .count(),
            2
        );
        let route = effects.iter().find(|effect| matches!(&effect.effect,
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. })) if jid == &recipient))
            .expect("recipient route is planned");
        assert!(route.dependencies.iter().any(|dependency| matches!(dependency,
            crate::server::routes::interpret::effects::PlanEffectDependency::AfterArchive { archive, .. }
                if archive == &recipient.to_bare())),
            "private-message delivery waits for its recipient archive");
        assert!(effects.iter().any(|effect| matches!(
            effect.effect,
            Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::Carbons { .. }
            ))
        )));
    }

    #[tokio::test]
    async fn plan_muc_decline_preserves_ledger_without_delivery() {
        use crate::server::routes::interpret::effects::PlanSink;
        use crate::server::routes::websocket::muc_invites::{
            list_invites, record_invite, OutstandingInvite,
        };
        let state = create_test_websocket_state().await;
        let decliner: jid::FullJid = "bob@example.com/web".parse().expect("decliner");
        let inviter: jid::FullJid = "alice@example.com/web".parse().expect("inviter");
        let invite = OutstandingInvite {
            room: "room@muc.example.com".parse().expect("room"),
            invitee: decliner.to_bare(),
            inviter: inviter.to_bare(),
        };
        let actor = state.deps.app_state.db_pool.global_actor().clone();
        record_invite(actor.clone(), &invite)
            .await
            .expect("seed invitation");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let _owner = register_test_connection(state.as_ref(), &inviter, tx).await;
        let mut message = Message::new(Some(invite.room.clone().into()));
        message.type_ = MessageType::Normal;
        message.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .build(),
                )
                .build(),
        );
        let sink = PlanSink::new();
        let base = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        );
        let deps = crate::server::routes::interpret::build_plan_deps(&base, &sink);
        let frames = handle_muc_direct_message(&message, state.as_ref(), &decliner, &deps)
            .await
            .expect("handled");
        assert!(frames.is_empty());
        assert!(rx.try_recv().is_err(), "planning must not deliver decline");
        assert_eq!(
            list_invites(actor, &invite.room, &invite.invitee)
                .await
                .expect("read invites"),
            vec![invite.clone()]
        );
        assert!(sink.snapshot().iter().any(|effect| matches!(&effect.effect,
            Effect::External(ExternalEffect::InviteLedger(super::super::muc_invite::InviteLedgerMutation::Claim { invite: planned })) if planned == &invite)));
        assert!(sink.snapshot().iter().any(|effect| effect.dependencies.contains(
            &crate::server::routes::interpret::effects::PlanEffectDependency::AfterInviteLedger { invite: invite.clone() }
        )), "decline delivery must depend on successful ledger claim");
    }
    #[tokio::test]
    async fn plan_bodyless_no_copy_pm_retains_canonical_envelope() {
        let state = create_test_websocket_state().await;
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let actor = create_test_room(state.as_ref(), room.clone()).await;
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let recipient: jid::FullJid = "bob@example.com/web".parse().expect("recipient");
        for (nick, jid) in [("alice", &sender), ("bob", &recipient)] {
            actor
                .ask(Join {
                    nick: nick.to_owned(),
                    real_jid: jid.clone(),
                    role: Role::Participant,
                    affiliation: Affiliation::Member,
                })
                .await
                .expect("join");
        }
        let mut incoming = pm();
        incoming.bodies.clear();
        incoming.payloads.push(
            minidom::Element::builder("no-copy", waddle_xmpp::xep::xep0334::NS_HINTS).build(),
        );
        incoming
            .payloads
            .push(minidom::Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN).build());
        incoming
            .payloads
            .push(waddle_xmpp::xep::xep0421::build_occupant_id_element(
                &OccupantId::new("forged-id"),
            ));
        let target = incoming.to.clone();
        let dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
        let mut machine = waddle_xmpp::protocol::XmppStateMachine::new("example.com", dispatcher);
        machine.transition_to_ready(sender.clone(), false);
        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        );
        let plan =
            crate::server::routes::interpret::plan_message_dispatch(&mut machine, incoming, &deps)
                .await;
        assert!(plan.error_reply.is_none());
        assert_eq!(plan.sanitized_message.from, Some(sender.clone().into()));
        assert_eq!(plan.sanitized_message.to, target);
        assert!(plan.sanitized_message.bodies.is_empty());
        assert_eq!(
            extract_occupant_id_from_message(&plan.sanitized_message),
            Some(generate_occupant_id(
                &sender.to_bare(),
                &room,
                &state.deps.occupant_id_secret
            ))
        );
        let muc_payloads: Vec<_> = plan
            .sanitized_message
            .payloads
            .iter()
            .filter(|payload| waddle_xmpp::muc::is_muc_service_namespace(payload.ns().as_str()))
            .collect();
        assert_eq!(muc_payloads.len(), 1);
        assert!(muc_payloads[0].is("x", waddle_xmpp::muc::presence::NS_MUC_USER));
        assert_eq!(muc_payloads[0].children().count(), 0);
        assert!(plan
            .sanitized_message
            .payloads
            .iter()
            .any(|payload| payload.is("no-copy", waddle_xmpp::xep::xep0334::NS_HINTS)));
        assert!(!plan.plan.iter().any(|effect| matches!(effect.effect, Effect::Durable(_)
            | Effect::External(ExternalEffect::Delivery(crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect::Carbons { .. })))));
    }
}
