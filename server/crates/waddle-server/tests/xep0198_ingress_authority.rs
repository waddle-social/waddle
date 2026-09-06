//! XEP-0198 §4–§6 ingress persistence and response reconstruction.
//! Private connection-loop crash/ACK seams are exercised in websocket::tests::
//! ingress_authority::recovery; these backend twins exercise real transactions.

pub mod ingress_support;
use ingress_support::IngressFixture;
use waddle_server::{
    ingress::effects::{PlanRejection, SemanticMalformedReason},
    ingress::{
        commit::commit_submission, execute::execute_effects, Deps, ImmediateSink,
        IngressDecisionClass, IngressStreamIdentity, IngressSubmission,
    },
    ingress_substrate::MessageEnvelope,
    ingress_uow::{CanonicalMessageRepository, SmIngressRepository, SmIngressStreamRepository},
};
use waddle_xmpp::{
    ingress::{
        FrozenStanzaError, IngressEffectIntent, IngressOrdinal, MessageKey, WireHandledCount,
    },
    pending_delivery::SmSessionId,
    Stanza,
};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

async fn rejection(fixture: &IngressFixture, position: u32, checkpoint: u32) -> IngressSubmission {
    let stream_id = SmSessionId::new("authority-recovery");
    let mut tx = fixture.uow.begin().await.expect("enroll");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("enroll commit");
    let mut submission = fixture.submission(None, "offered stanza");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(position),
        checkpoint_h: WireHandledCount::new(checkpoint),
    };
    let error = StanzaError::new(
        ErrorType::Modify,
        DefinedCondition::BadRequest,
        "en",
        "malformed payload",
    );
    let mut reply = submission.plan.sanitized_message.clone();
    reply.to = reply.from.take();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.payloads.push(error.clone().into());
    submission.plan.error_reply = Some(Stanza::Message(reply));
    submission.plan.rejection = Some(PlanRejection::SemanticMalformed(
        SemanticMalformedReason::MalformedPayload,
    ));
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ErrorReply {
            recipient: submission.sender.clone(),
            error: FrozenStanzaError::from_xmpp(&error).expect("typed error"),
        });
    submission
}

async fn assert_error_wire(
    fixture: &IngressFixture,
    decision: &waddle_server::ingress::IngressDecision,
) {
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(1),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    let frames: Vec<_> = report
        .frame_obligations
        .iter()
        .flat_map(|entry| &entry.frames)
        .collect();
    assert_eq!(frames.len(), 1);
    let Stanza::Message(message) = frames[0] else {
        panic!("message error frame")
    };
    let element: minidom::Element = message.clone().into();
    let mut wire = Vec::new();
    element.write_to(&mut wire).expect("wire serialization");
    let decoded: minidom::Element = std::str::from_utf8(&wire)
        .expect("wire UTF8")
        .parse()
        .expect("wire XML");
    assert_eq!(decoded.attr("type"), Some("error"));
    let error = decoded
        .get_child("error", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
        .expect("stanza error");
    assert!(error
        .get_child("bad-request", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        0,
        "frame has not been written by a transport"
    );
    assert!(fixture
        .optional_text("SELECT CAST(terminal_at AS TEXT) FROM ingress_messages")
        .await
        .is_none());
}

async fn crash_before_commit(fixture: IngressFixture) {
    let submission = rejection(&fixture, 1, 1).await;
    let key = MessageKey::new();
    let digest = waddle_xmpp::ingress::digest::v1::digest(&submission.digest_input);
    let envelope =
        MessageEnvelope::new(submission.plan.sanitized_message.clone()).expect("envelope");
    {
        let mut tx = fixture.uow.begin().await.expect("doomed transaction");
        CanonicalMessageRepository::record_message(&mut tx, key, &digest, Some(&envelope))
            .await
            .expect("uncommitted row");
        // Dropping the actual unit of work models losing the process before COMMIT.
    }
    assert_eq!(fixture.count("ingress_messages").await, 0);
    let committed = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("retransmission");
    assert_eq!(committed.class, IngressDecisionClass::SemanticMalformed);
    assert_eq!(committed.ordinal, Some(IngressOrdinal::FIRST));
    assert_ne!(committed.message_key, Some(key));
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    assert_error_wire(&fixture, &committed).await;
    fixture.close().await;
}

async fn replay_pending_position(fixture: IngressFixture) {
    let submission = rejection(&fixture, 3, 0).await;
    let first = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("first commit");
    let replay = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("ambiguous retry");
    assert_eq!(replay.class, IngressDecisionClass::ExistingCommitted);
    assert_eq!(replay.message_key, first.message_key);
    assert_eq!(replay.ordinal, first.ordinal);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    let IngressStreamIdentity::Resumable { sm_ingress_id, .. } = submission.identity else {
        panic!("resumable")
    };
    let mut tx = fixture.uow.begin().await.expect("checkpoint read");
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, sm_ingress_id)
            .await
            .expect("checkpoint"),
        Some(WireHandledCount::new(0))
    );
    assert_eq!(
        SmIngressRepository::lookup_wire_binding(&mut tx, sm_ingress_id, WireHandledCount::new(3))
            .await
            .expect("binding"),
        Some((first.message_key.expect("canonical"), IngressOrdinal::FIRST))
    );
    tx.commit().await.expect("read complete");
    assert_error_wire(&fixture, &replay).await;
    fixture.close().await;
}

/// XEP-0198 §4–§5: an uncommitted stanza remains the sender's retransmission obligation.
#[tokio::test]
async fn crash_before_commit_sqlite() {
    crash_before_commit(IngressFixture::sqlite().await).await;
}
/// XEP-0198 §4–§5: PostgreSQL counterpart of the dropped-transaction crash.
#[tokio::test]
async fn crash_before_commit_postgres() {
    if let Some(fixture) = IngressFixture::postgres("sm_crash_before").await {
        crash_before_commit(fixture).await;
    }
}
/// XEP-0198 §4–§5: retransmitting a committed position behind an IQ hole preserves identity.
#[tokio::test]
async fn ambiguous_pending_wire_position_sqlite() {
    replay_pending_position(IngressFixture::sqlite().await).await;
}
/// XEP-0198 §4–§5: PostgreSQL counterpart of a committed retransmission behind a hole.
#[tokio::test]
async fn ambiguous_pending_wire_position_postgres() {
    if let Some(fixture) = IngressFixture::postgres("sm_pending_replay").await {
        replay_pending_position(fixture).await;
    }
}
