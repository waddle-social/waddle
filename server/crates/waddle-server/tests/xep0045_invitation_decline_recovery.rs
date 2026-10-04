//! XEP-0045 §7.8.2 decline recovery through the public server test-support API.

#![cfg(feature = "test-support")]

pub mod ingress_support;

use ingress_support::IngressFixture;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use waddle_server::{
    db::{DatabaseConfig, DatabasePool, PoolConfig},
    ingress::{commit::commit_submission, RecoveryEnvironment},
    ingress_substrate::EffectReceiptKind,
    ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository},
    test_support::{
        claim_invite, create_test_session, list_invites, record_invite_at,
        register_test_connection, websocket_state_with_ingress, OutstandingInvite, RecordOutcome,
    },
};
use waddle_xmpp::{ingress::IngressEffectIntent, Stanza};

#[derive(Clone, Copy)]
enum PartialDeclineReceipt {
    Route,
    Fallback,
}

/// Wait until a full maintenance pass completed after `passes_before` and the
/// expected number of canonical rows is terminal. Terminalization runs before
/// recovery inside one pass, so the terminal count alone would not prove the
/// pass's recovery phase finished.
async fn wait_for_pass(
    metrics: &waddle_xmpp::telemetry::test_support::MetricsTestGuard,
    passes_before: u64,
    fixture: &IngressFixture,
    expected_terminal: i64,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let passes = metrics
                .counter_sum("ingress.maintenance.runs", &[("phase", "pass")])
                .unwrap_or(0);
            if passes > passes_before
                && fixture
                    .count("ingress_messages WHERE terminal_at IS NOT NULL")
                    .await
                    == expected_terminal
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance pass terminalizes the recovered submission");
}

async fn age_nonterminal_rows(fixture: &IngressFixture) {
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => {
            "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL"
        }
        waddle_server::db::DatabaseDriver::Sqlite => {
            "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL"
        }
    };
    fixture
        .execute(
            sql,
            waddle_server::db_params![
                (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339()
            ],
        )
        .await;
}

async fn assert_family_recovered(fixture: &IngressFixture, key: waddle_xmpp::ingress::MessageKey) {
    let mut transaction = fixture.uow.begin().await.expect("inspect recovery");
    assert!(
        EffectReceiptRepository::receipts_complete(&mut transaction, key)
            .await
            .expect("receipts complete")
    );
    assert!(
        CanonicalMessageRepository::is_terminal(&mut transaction, key)
            .await
            .expect("terminal")
    );
    transaction.commit().await.expect("inspection commit");
    let receipts = fixture
        .optional_text(&format!(
            "SELECT CAST(COUNT(*) AS TEXT) FROM ingress_effect_receipts WHERE message_key = '{}'",
            key.to_storage()
        ))
        .await
        .expect("receipt count");
    assert_eq!(receipts.parse::<i64>().expect("integer receipt count"), 3);
}

async fn muc_decline_recovery(
    fixture: IngressFixture,
    partial: Option<PartialDeclineReceipt>,
    reinvited: bool,
) {
    let pool = Arc::new(
        DatabasePool::new(
            DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            PoolConfig,
        )
        .await
        .expect("shared database pool"),
    );
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let authority = Arc::new(fixture.authority().await);
    let state = websocket_state_with_ingress(pool, Arc::clone(&authority)).await;
    let environment: Arc<dyn RecoveryEnvironment> = state.clone();
    authority.bind_recovery_environment(Arc::downgrade(&environment));
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
    submission.plan =
        waddle_server::server::plan_message_dispatch(&mut machine, message, &state.recovery_deps())
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
    // Age the row past the default maintenance grace before reading its canonical
    // timestamp, so the reinvited case still orders the replacement after the
    // original invitation.
    age_nonterminal_rows(&fixture).await;
    let mut transaction = fixture
        .uow
        .begin()
        .await
        .expect("inspect canonical receipt");
    let canonical_created_at = CanonicalMessageRepository::created_at(&mut transaction, key)
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
        let kind = EffectReceiptKind::from_storage(
            intent
                .with_encoded_v1(|kind, _| kind)
                .expect("receipt storage kind"),
        );
        let semantic_identity_hash: [u8; 32] =
            Sha256::digest(intent.semantic_key().storage_identity()).into();
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            kind,
            &semantic_identity_hash,
        )
        .await
        .expect("partial receipt");
    }
    let replacement_created_at = canonical_created_at - chrono::Duration::minutes(1);
    assert!(replacement_created_at > invitation_created_at);
    if reinvited {
        assert!(claim_invite(actor.clone(), &invite)
            .await
            .expect("competing decline"));
        assert!(matches!(
            // Created after canonical intake, but an app clock behind the database
            // timestamps it before that receipt. Only the observed generation
            // can prevent this old decline from consuming the replacement.
            record_invite_at(actor.clone(), &invite, replacement_created_at)
                .await
                .expect("new invitation"),
            RecordOutcome::New { .. }
        ));
    }
    let passes_before = metrics
        .counter_sum("ingress.maintenance.runs", &[("phase", "pass")])
        .unwrap_or(0);
    authority.trigger_maintenance();
    wait_for_pass(&metrics, passes_before, &fixture, 1).await;
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
        let Stanza::Message(message) = delivered.stanza else {
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
    assert_family_recovered(&fixture, key).await;

    let sentinel = fixture.submission(Some("decline-second-pass"), "sentinel");
    commit_submission(&fixture.uow, &sentinel, 5)
        .await
        .expect("commit sentinel");
    age_nonterminal_rows(&fixture).await;
    let passes_before = metrics
        .counter_sum("ingress.maintenance.runs", &[("phase", "pass")])
        .unwrap_or(0);
    authority.trigger_maintenance();
    wait_for_pass(&metrics, passes_before, &fixture, 2).await;
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
    assert_family_recovered(&fixture, key).await;
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(environment);
    drop(state);
    drop(authority);
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
