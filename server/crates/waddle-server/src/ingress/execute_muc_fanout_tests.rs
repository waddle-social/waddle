#![cfg(feature = "clustering")]

use super::super::groupchat_receipt_tests::groupchat_decision;
use super::*;
use crate::ingress::{test_support::IngressFixture, RouteProgress};
use crate::server::routes::interpret::effects::{
    delivery::PeerDeliveryKind, Effect, PlanEffectDependency,
};
use crate::server::routes::websocket::{
    handlers::message::muc_invite::InviteLedgerMutation, muc_invites::OutstandingInvite,
};
use waddle_xmpp::ingress::IngressEffectIntent;

fn planned(decision: &IngressDecision) -> Vec<PlannedEffect> {
    decision
        .external
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, effect)| {
            let mut plan = PlannedEffect::new(Effect::External(effect));
            plan.dependencies = decision
                .external_dependencies
                .get(index)
                .cloned()
                .unwrap_or_default();
            plan
        })
        .collect()
}

#[tokio::test]
async fn only_unfinished_local_muc_copies_pass_the_remote_copy() {
    let fixture = IngressFixture::sqlite().await;
    let mut decision = groupchat_decision(&fixture).await;
    assert_eq!(
        local_before_remote(&decision, &planned(&decision), &[None; 3], 1),
        Some(0)
    );
    for completed in [Some(true), Some(false)] {
        assert_eq!(
            local_before_remote(&decision, &planned(&decision), &[completed, None, None], 1),
            None
        );
    }
    assert_eq!(
        local_before_remote(&decision, &planned(&decision), &[None; 3], 2),
        None,
        "reflection is not a remote fanout blocker"
    );
    let ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
        route_identity,
        jid,
        stanza,
        call_setup,
        ..
    }) = decision.external[0].clone()
    else {
        panic!("local copy")
    };
    decision.external[0] = ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
        route_identity,
        call_setup,
        bare: jid.to_bare(),
        resources: vec![jid],
        stanza,
    });
    assert_eq!(
        local_before_remote(&decision, &planned(&decision), &[None; 3], 1),
        Some(0),
        "detached local progress is independent too"
    );

    // A sender reflection expressed as peer delivery still has no occupant receipt.
    let IngressEffectIntent::RouteMucGroupchat { reflection, .. } =
        decision.route_progress[0].settle_evidence()
    else {
        panic!("groupchat")
    };
    let ExternalEffect::Frame(stanza) = decision.external[2].clone() else {
        panic!("reflection")
    };
    decision.external[2] = ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
        route_identity: None,
        jid: reflection,
        stanza,
        kind: PeerDeliveryKind::PeerStanza,
        call_setup: None,
    });
    assert_eq!(
        local_before_remote(&decision, &planned(&decision), &[Some(true), None, None], 1),
        None,
        "reflection must not pass as occupant progress"
    );
    fixture.close().await;
}

#[tokio::test]
async fn local_copy_waits_for_its_actual_dependency_to_succeed() {
    let fixture = IngressFixture::sqlite().await;
    let mut decision = groupchat_decision(&fixture).await;
    let invite = OutstandingInvite {
        room: "room@muc.example.com".parse().unwrap(),
        invitee: "juliet@example.com".parse().unwrap(),
        inviter: "romeo@example.com".parse().unwrap(),
    };
    decision
        .external
        .push(ExternalEffect::InviteLedger(InviteLedgerMutation::Claim {
            message_key: None,
            not_after: None,
            invite: invite.clone(),
        }));
    let mut plans = planned(&decision);
    plans[0]
        .dependencies
        .push(PlanEffectDependency::AfterInviteLedger { invite });
    assert_eq!(
        local_before_remote(&decision, &plans, &[None; 4], 1),
        None,
        "pending predecessor cannot be bypassed"
    );
    assert_eq!(
        local_before_remote(&decision, &plans, &[None, None, None, Some(false)], 1),
        None,
        "failed predecessor cannot be bypassed"
    );
    assert_eq!(
        local_before_remote(&decision, &plans, &[None, None, None, Some(true)], 1),
        Some(0)
    );
    decision.external.pop();
    plans.pop();
    assert_eq!(
        local_before_remote(&decision, &plans, &[None; 3], 1),
        None,
        "missing predecessor fails closed"
    );
    fixture.close().await;
}

#[tokio::test]
async fn different_muc_receipts_do_not_share_scheduling_priority() {
    let fixture = IngressFixture::sqlite().await;
    let mut decision = groupchat_decision(&fixture).await;
    let other_room: jid::BareJid = "other@muc.example.com".parse().unwrap();
    let stamp =
        waddle_xmpp_core::xep0359::StanzaId::new("other-message", other_room.clone().into());
    let mut intent = decision.route_progress[0].settle_evidence();
    let IngressEffectIntent::RouteMucGroupchat {
        room,
        route_identity,
        ..
    } = &mut intent
    else {
        panic!("groupchat")
    };
    *room = other_room.clone();
    *route_identity = waddle_xmpp::ingress::EffectMessageIdentity::stanza(stamp.clone());
    let ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { stanza, .. }) =
        &mut decision.external[0]
    else {
        panic!("local")
    };
    let waddle_xmpp::Stanza::Message(message) = stanza.as_mut() else {
        panic!("groupchat message")
    };
    message.from = Some(other_room.with_resource_str("romeo").unwrap().into());
    message.payloads.clear();
    waddle_xmpp_core::xep0359::add_stanza_id(message, &stamp);
    let separate =
        RouteProgress::from_intent(&intent, decision.route_progress[0].received_at, Vec::new())
            .unwrap()
            .unwrap();
    assert_ne!(decision.route_progress[0].receipt, separate.receipt);
    assert!(decision.route_progress[0].matches(&decision.external[1]));
    assert!(separate.matches(&decision.external[0]));
    decision.route_progress.push(separate);
    assert_eq!(
        local_before_remote(&decision, &planned(&decision), &[None; 3], 1),
        None,
        "individually matched copies do not imply a shared MUC receipt"
    );
    fixture.close().await;
}
