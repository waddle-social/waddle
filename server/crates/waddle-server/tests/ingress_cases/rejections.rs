use super::*;

async fn semantic_rejections(fixture: IngressFixture) {
    use waddle_xmpp::{ingress::FrozenStanzaError, Stanza};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
    for (index, condition, class) in [
        (
            0_i64,
            DefinedCondition::BadRequest,
            IngressDecisionClass::SemanticMalformed,
        ),
        (
            1,
            DefinedCondition::NotAuthorized,
            IngressDecisionClass::AuthorizationDenied,
        ),
        (
            2,
            DefinedCondition::NotAcceptable,
            IngressDecisionClass::PolicyDenied,
        ),
        (
            3,
            DefinedCondition::ResourceConstraint,
            IngressDecisionClass::CaptureOverflow,
        ),
    ] {
        let mut submission = fixture.submission(Some("rejected-origin"), "offered body");
        // Rejected plans carry no local room writes to revalidate; semantic
        // denials still commit when planning observed a local room snapshot.
        submission.plan.room_execution = waddle_server::ingress::RoomExecutionPath::Local {
            room: "room@conference.example.com".parse().expect("room"),
            fence: waddle_server::ingress::effects::room::RoomFenceRequirement::Unfenced,
            snapshot_generation: 1,
        };
        let error = StanzaError::new(ErrorType::Cancel, condition, "en", "rejected");
        let mut reply = submission.plan.sanitized_message.clone();
        reply.type_ = xmpp_parsers::message::MessageType::Error;
        reply.to = reply.from.take();
        reply.payloads.push(error.clone().into());
        submission.plan.error_reply = Some(Stanza::Message(reply));
        submission
            .plan
            .intents
            .push(IngressEffectIntent::ErrorReply {
                recipient: "romeo@example.com/phone".parse().expect("reply recipient"),
                error: FrozenStanzaError::from_xmpp(&error).expect("frozen error"),
            });
        let decision = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("commit standard error");
        assert_eq!(decision.class, class);
        assert!(decision.class.advances());
        assert_eq!(decision.external.len(), 1);
        assert_eq!(fixture.count("ingress_messages").await, index + 1);
        assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
        assert_eq!(fixture.count("ingress_effect_intents").await, index + 1);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
        assert_eq!(fixture.count("ingress_sm_refs").await, 0);
        let mut tx = fixture.uow.begin().await.expect("read denial envelope");
        assert_eq!(
            CanonicalMessageRepository::load_envelope(
                &mut tx,
                decision.message_key.expect("denial canonical")
            )
            .await
            .expect("denial envelope"),
            Some(
                MessageEnvelope::new(submission.plan.sanitized_message).expect("offered envelope")
            )
        );
        tx.commit().await.expect("close read");
    }
    fixture.close().await;
}
#[tokio::test]
async fn ingress_semantic_rejections_sqlite() {
    semantic_rejections(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_semantic_rejections_postgres() {
    if let Some(fixture) = IngressFixture::postgres("semantic_rejections").await {
        semantic_rejections(fixture).await;
    }
}
