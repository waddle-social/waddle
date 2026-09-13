//! Dedicated XEP-0045 suite: recovery of a lost mediated-invitation decline.
//!
//! XEP-0045 §7.8.2 (Mediated Invitation): when the invitee declines, the room
//! forwards `<x xmlns='http://jabber.org/protocol/muc#user'><decline from=…/>`
//! to the inviter, preserving the reason, and the invitation is consumed. These
//! cases drive the real decline planner and invite ledger, commit Phase B, lose
//! Phase C, and prove the maintenance recovery phase delivers that payload
//! exactly once while consuming the invitation. They live inside the crate
//! because the decline arm needs the in-process WebSocket state (invite ledger
//! actor), which no public integration fixture can build today.
use super::family_tests::{family_pass, family_recovered, family_state};
use crate::{
    ingress::{
        commit::commit_submission,
        maintenance::{MaintenanceCursor, MaintenanceOutcome},
        test_support::IngressFixture,
    },
    server::routes::websocket::interpret_loop::build_interpret_deps,
};
use waddle_xmpp::ingress::IngressEffectIntent;

/// Which of the mutually exclusive invitation receipts an interrupted
/// executor persisted before losing the rest of Phase C.
#[derive(Clone, Copy)]
enum PartialDeclineReceipt {
    Route,
    Fallback,
}

async fn muc_decline_recovery(
    fixture: IngressFixture,
    partial: Option<PartialDeclineReceipt>,
    reinvited: bool,
) {
    use crate::server::routes::websocket::{
        muc_invites::{list_invites, record_invite_at, OutstandingInvite},
        tests::{create_test_session, register_test_connection},
    };
    let state = family_state(&fixture).await;
    create_test_session(state.as_ref(), "romeo").await;
    create_test_session(state.as_ref(), "juliet").await;
    let inviter: jid::FullJid = "juliet@example.com/phone".parse().expect("inviter");
    let mut submission = fixture.submission(Some("decline-lost-phase-c"), "");
    let invite = OutstandingInvite {
        room: "room@muc.example.com".parse().expect("room"),
        invitee: submission.sender.to_bare(),
        inviter: inviter.to_bare(),
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let invitation_created_at = chrono::Utc::now() - chrono::Duration::hours(1);
    record_invite_at(actor.clone(), &invite, invitation_created_at)
        .await
        .expect("seed invitation");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let _owner = register_test_connection(state.as_ref(), &inviter, tx).await;
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(invite.room.clone());
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(invite.room.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Normal;
    message.bodies.clear();
    let ns = waddle_xmpp::muc::presence::NS_MUC_USER;
    message.payloads.push(
        minidom::Element::builder("x", ns)
            .append(
                minidom::Element::builder("decline", ns)
                    .attr(
                        minidom::rxml::xml_ncname!("to").to_owned(),
                        inviter.to_bare(),
                    )
                    .append(
                        minidom::Element::builder("reason", ns)
                            .append("cannot join")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.sender.to_bare(), invite.room.clone()],
            stanza_lang: None,
        },
    )
    .expect("decline digest");
    let mut dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = waddle_xmpp::protocol::XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(submission.sender.clone(), false);
    submission.plan = crate::server::plan_message_dispatch(
        &mut machine,
        message,
        &build_interpret_deps(state.as_ref(), None),
    )
    .await;
    assert!(
        submission
            .plan
            .intents
            .iter()
            .any(|intent| matches!(intent, IngressEffectIntent::MucInviteLedger { .. })),
        "real planner must capture decline claim"
    );
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit decline");
    let key = decision.message_key.expect("key");
    let mut transaction = fixture
        .uow
        .begin()
        .await
        .expect("inspect canonical receipt");
    let canonical_created_at =
        crate::ingress_uow::CanonicalMessageRepository::created_at(&mut transaction, key)
            .await
            .expect("canonical receipt time");
    transaction
        .commit()
        .await
        .expect("finish receipt inspection");
    let claimed_generation = submission
        .plan
        .intents
        .iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::MucInviteLedger { mutation } => mutation.recorded_at,
            _ => None,
        });
    assert_eq!(claimed_generation, Some(invitation_created_at));
    assert_eq!(
        list_invites(actor.clone(), &invite.room, &invite.invitee)
            .await
            .expect("unclaimed invite"),
        vec![(invite.clone(), invitation_created_at)]
    );
    assert!(rx.try_recv().is_err());
    if let Some(partial) = partial {
        // Model an executor that delivered the decline live, persisted one
        // of the two mutually exclusive receipts and then lost the rest.
        let inviter_bare = inviter.to_bare();
        let intent = submission
            .plan
            .intents
            .iter()
            .find(|intent| match (partial, intent) {
                (
                    PartialDeclineReceipt::Route,
                    IngressEffectIntent::RouteDirect { recipient, .. },
                ) => recipient == &inviter_bare,
                (
                    PartialDeclineReceipt::Fallback,
                    IngressEffectIntent::PendingDelivery {
                        mutation:
                            waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                                recipient, ..
                            },
                    },
                ) => recipient == &inviter_bare,
                _ => false,
            })
            .expect("recorded inviter delivery intent");
        let receipt = crate::ingress::receipt_key(intent).expect("receipt key");
        crate::ingress_uow::EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("partial receipt");
    }
    let replacement_created_at = canonical_created_at - chrono::Duration::minutes(1);
    assert!(replacement_created_at > invitation_created_at);
    if reinvited {
        // A competing decline consumed the original invitation. An unexpired
        // record is not replaced by record_invite_at, so consume it first.
        assert!(crate::server::routes::websocket::muc_invites::claim_invite(
            actor.clone(),
            &invite,
        )
        .await
        .expect("competing decline"));
        assert!(matches!(
            // Created after canonical intake, but an app clock behind the database
            // timestamps it before that receipt. Only the observed generation
            // can prevent this old decline from consuming the replacement.
            record_invite_at(
                actor.clone(),
                &invite,
                canonical_created_at - chrono::Duration::minutes(1)
            )
            .await
            .expect("new invitation"),
            crate::server::routes::websocket::muc_invites::RecordOutcome::New { .. }
        ));
    }
    let cursor = MaintenanceCursor::default();
    assert_eq!(
        family_pass(&fixture, &state, &cursor).await,
        MaintenanceOutcome::Complete
    );
    if reinvited {
        assert!(
            rx.try_recv().is_err(),
            "old decline cannot forward for a new invitation"
        );
    } else if partial.is_some() {
        assert!(
            rx.try_recv().is_err(),
            "one committed invitation receipt proves the delivery; recovery must not resend"
        );
    } else {
        let delivered = rx.try_recv().expect("inviter receives recovered decline");
        let waddle_xmpp::Stanza::Message(message) = delivered.stanza else {
            panic!("decline message")
        };
        assert_eq!(message.from, Some(invite.room.clone().into()));
        let decline = message
            .payloads
            .iter()
            .find_map(|payload| payload.get_child("decline", ns))
            .expect("XEP-0045 decline payload");
        assert_eq!(
            decline.attr("from"),
            Some(invite.invitee.to_string().as_str())
        );
        assert_eq!(
            decline.get_child("reason", ns).expect("reason").text(),
            "cannot join"
        );
    }
    assert_eq!(
        list_invites(actor.clone(), &invite.room, &invite.invitee)
            .await
            .expect("invitation after recovery"),
        if reinvited {
            vec![(invite.clone(), replacement_created_at)]
        } else {
            vec![]
        }
    );
    assert_eq!(
        fixture.count("muc_invite_claims WHERE claimed = 1").await,
        i64::from(!reinvited)
    );
    assert_eq!(
        fixture.count("muc_invite_claims WHERE claimed = 0").await,
        i64::from(reinvited)
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "live inviter settles the recorded fallback without queueing"
    );
    assert!(rx.try_recv().is_err());
    family_recovered(&fixture, key, 3).await;
    assert_eq!(
        family_pass(&fixture, &state, &cursor).await,
        MaintenanceOutcome::Complete
    );
    assert!(rx.try_recv().is_err(), "second pass cannot resend decline");
    assert_eq!(
        list_invites(actor, &invite.room, &invite.invitee)
            .await
            .expect("invitation remains resolved"),
        if reinvited {
            vec![(invite, replacement_created_at)]
        } else {
            vec![]
        }
    );
    assert_eq!(
        fixture.count("muc_invite_claims WHERE claimed = 1").await,
        i64::from(!reinvited)
    );
    assert_eq!(
        fixture.count("muc_invite_claims WHERE claimed = 0").await,
        i64::from(reinvited)
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "live inviter settles the recorded fallback without queueing"
    );
    family_recovered(&fixture, key, 3).await;
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_decline_recovers_claim_and_inviter_route() {
    muc_decline_recovery(IngressFixture::sqlite().await, None, false).await;
}
#[tokio::test]
async fn postgres_muc_decline_recovers_claim_and_inviter_route() {
    if let Some(fixture) = IngressFixture::postgres("recovery_muc_decline").await {
        muc_decline_recovery(fixture, None, false).await;
    }
}
#[tokio::test]
async fn sqlite_muc_decline_route_receipt_alone_settles_without_resend() {
    muc_decline_recovery(
        IngressFixture::sqlite().await,
        Some(PartialDeclineReceipt::Route),
        false,
    )
    .await;
}
#[tokio::test]
async fn postgres_muc_decline_route_receipt_alone_settles_without_resend() {
    if let Some(fixture) = IngressFixture::postgres("recovery_muc_decline_route").await {
        muc_decline_recovery(fixture, Some(PartialDeclineReceipt::Route), false).await;
    }
}
#[tokio::test]
async fn sqlite_muc_decline_fallback_receipt_alone_settles_without_resend() {
    muc_decline_recovery(
        IngressFixture::sqlite().await,
        Some(PartialDeclineReceipt::Fallback),
        false,
    )
    .await;
}
#[tokio::test]
async fn postgres_muc_decline_fallback_receipt_alone_settles_without_resend() {
    if let Some(fixture) = IngressFixture::postgres("recovery_muc_decline_fallback").await {
        muc_decline_recovery(fixture, Some(PartialDeclineReceipt::Fallback), false).await;
    }
}

#[tokio::test]
async fn sqlite_muc_decline_recovery_preserves_newer_invitation() {
    muc_decline_recovery(IngressFixture::sqlite().await, None, true).await;
}
#[tokio::test]
async fn postgres_muc_decline_recovery_preserves_newer_invitation() {
    if let Some(fixture) = IngressFixture::postgres("recovery_muc_decline_newer").await {
        muc_decline_recovery(fixture, None, true).await;
    }
}
