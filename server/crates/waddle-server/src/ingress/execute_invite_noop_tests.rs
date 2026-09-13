//! XEP-0045 repeated invitations discharge the delivery excluded by ledger deduplication.
use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::{
    interpret::effects::{invite::MucUserRoute, PlanEffectDependency, PlanSuppressionPolicy},
    websocket::{
        handlers::message::muc_invite::InviteLedgerMutation,
        interpret_loop::build_interpret_deps,
        muc_invites::{self, OutstandingInvite, RecordOutcome},
        tests as socket_tests,
    },
};
use std::sync::Arc;
use waddle_xmpp::{
    ingress::{
        EffectMessageIdentity, IngressEffectIntent, MucInviteLedgerAction, MucInviteLedgerMutation,
        PendingDeliveryMutation,
    },
    pending_delivery::{PendingPayload, PendingRow, PendingRowId},
};

async fn outstanding_invite_discharges_delivery(fixture: IngressFixture) {
    let standalone = socket_tests::create_test_websocket_state().await;
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    let state = socket_tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        Arc::clone(&standalone.deps.protocol.ingress),
    )
    .await;
    let invite = OutstandingInvite {
        room: "room@muc.example.com".parse().expect("room"),
        invitee: "juliet@example.com".parse().expect("invitee"),
        inviter: fixture.principal.bare_jid().clone(),
    };
    let ledger = state.deps.app_state.db_pool.global_actor().clone();
    assert!(matches!(
        muc_invites::record_invite_at(ledger.clone(), &invite, chrono::Utc::now())
            .await
            .expect("first invitation"),
        RecordOutcome::New { .. }
    ));
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    socket_tests::register_test_connection(&state, &resource, tx).await;

    let mut submission = fixture.submission(Some("fresh-origin-repeated-invite"), "invite");
    let recorded_at = chrono::Utc::now();
    let row_id = PendingRowId::fresh();
    let route_identity = EffectMessageIdentity::CaptureOrdinal(7);
    let route = MucUserRoute {
        route_identity: Some(route_identity.clone()),
        recipient: invite.invitee.clone(),
        resources: vec![resource.clone()],
        message: Box::new(submission.plan.sanitized_message.clone()),
        fallback: PendingRow {
            id: row_id.clone(),
            recipient: invite.invitee.clone(),
            original_receipt_at: recorded_at,
            payload: PendingPayload::Transient(Box::new(submission.plan.sanitized_message.clone())),
            flushed_in_session: None,
            outbound_sequence: None,
        },
        failure: None,
    };
    let mut delivery = PlannedEffect::new(Effect::External(ExternalEffect::RouteToPeer(route)))
        .with_suppression(PlanSuppressionPolicy::SenderOnly);
    delivery
        .dependencies
        .push(PlanEffectDependency::AfterInviteLedger {
            invite: invite.clone(),
        });
    // Deliberately reverse order to exercise dependency scheduling.
    submission.plan.plan = vec![
        delivery,
        PlannedEffect::new(Effect::External(ExternalEffect::InviteLedger(
            InviteLedgerMutation::Record {
                invite: invite.clone(),
                recorded_at,
                failure: None,
            },
        ))),
    ];
    submission.plan.intents = vec![
        IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: invite.room.clone(),
                invitee: invite.invitee.clone(),
                inviter: invite.inviter.clone(),
                action: MucInviteLedgerAction::Recorded,
                recorded_at: Some(recorded_at),
            },
        },
        IngressEffectIntent::RouteDirect {
            recipient: invite.invitee.clone(),
            fanout: vec![resource],
            route_identity,
        },
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: invite.invitee.clone(),
                row_id,
            },
        },
    ];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit repeated invite");
    let key = decision.message_key.expect("canonical message");
    assert_eq!(fixture.count("ingress_effect_intents").await, 3);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    let deps = build_interpret_deps(&state, None);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert!(report.receipt_failures.is_empty());
    assert!(report.frame_obligations.is_empty());
    assert!(
        rx.try_recv().is_err(),
        "outstanding invite is not delivered again"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .count(&invite.invitee)
            .await
            .expect("pending rows"),
        0
    );
    assert_eq!(fixture.count("muc_pending_invites").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminalize no-op"));
    assert!(matches!(
        muc_invites::record_invite_at(ledger, &invite, chrono::Utc::now())
            .await
            .expect("retained original invitation"),
        RecordOutcome::AlreadyOutstanding
    ));
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_outstanding_invite_discharges_ledger_live_and_fallback_receipts() {
    outstanding_invite_discharges_delivery(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_outstanding_invite_discharges_ledger_live_and_fallback_receipts() {
    if let Some(fixture) = IngressFixture::postgres("invite_noop_receipts").await {
        outstanding_invite_discharges_delivery(fixture).await;
    }
}

#[test]
fn invite_noop_discharge_excludes_non_delivery_effects() {
    let invite = OutstandingInvite {
        room: "room@muc.example.com".parse().expect("room"),
        invitee: "juliet@example.com".parse().expect("invitee"),
        inviter: "romeo@example.com".parse().expect("inviter"),
    };
    let ledger = ExternalEffect::InviteLedger(InviteLedgerMutation::Claim {
        message_key: None,
        not_after: None,
        invite: invite.clone(),
    });
    let mut frame = PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
        Stanza::Message(xmpp_parsers::message::Message::new(None)),
    ))));
    frame
        .dependencies
        .push(PlanEffectDependency::AfterInviteLedger { invite });
    assert!(!is_invite_delivery_dependent(&ledger, &frame));
}
