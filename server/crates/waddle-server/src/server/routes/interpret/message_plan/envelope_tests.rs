use super::*;
use crate::ingress::{
    commit::commit_submission, execute::execute_effects, test_support::IngressFixture,
    IngressStreamIdentity,
};
use crate::ingress_uow::SmIngressStreamRepository;
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{
    ingress::WireHandledCount,
    mam::{MamStorage, SqlxMamStorage},
    pending_delivery::SmSessionId,
    registry::ConnectionRegistry,
};

async fn foreign_error_survives_exact_replay(fixture: IngressFixture) {
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let mut deps = Deps::registry_only(&registry);
    deps.mam_storage = Some(&mam);
    let mut submission = fixture.submission(Some("foreign-error"), "correction");
    let foreign = minidom::Element::builder("error", "urn:example:foreign-extension")
        .append("application detail")
        .build();
    submission
        .plan
        .sanitized_message
        .payloads
        .push(foreign.clone());
    submission.plan.sanitized_message.payloads.push(
        waddle_xmpp::xep::xep0308::build_replace_element("absent-target"),
    );
    let offered = submission.plan.sanitized_message.clone();
    let stream_id = SmSessionId::new("foreign-error-replay");
    let mut tx = fixture.uow.begin().await.expect("begin");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("commit stream");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(1),
        checkpoint_h: WireHandledCount::new(1),
    };
    let mut dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(submission.sender.clone(), false);
    submission.plan = plan_message_dispatch(&mut machine, offered.clone(), &deps).await;
    let original_reply = submission
        .plan
        .error_reply
        .clone()
        .expect("missing target rejected");
    assert!(submission
        .plan
        .sanitized_message
        .payloads
        .contains(&foreign));
    assert!(submission
        .plan
        .sanitized_message
        .payloads
        .iter()
        .all(
            |payload| xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).is_err()
        ));
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit denial");
    assert!(decision.class.advances());
    // Model cancellation before writing the first reply. Exact wire replay must
    // reconstruct its response from the committed envelope and recorded error.
    submission.plan = plan_message_dispatch(&mut machine, offered, &deps).await;
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("exact replay");
    assert_eq!(decision.message_key, replay.message_key);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &super::super::effects::ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.frame_obligations.len(), 1);
    assert_eq!(report.frame_obligations[0].frames.len(), 1);
    assert_eq!(
        report.frame_obligations[0].frames[0].to_element(),
        original_reply.to_element()
    );
    assert_eq!(
        fixture
            .count("ingress_sm_streams WHERE handled_ordinal = 1 AND checkpoint_h = 1")
            .await,
        1
    );
    drop(mam);
    fixture.close().await;
}

#[tokio::test]
async fn rejected_foreign_error_extension_exact_replay_sqlite() {
    foreign_error_survives_exact_replay(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn rejected_foreign_error_extension_exact_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("foreign_error_replay").await {
        foreign_error_survives_exact_replay(fixture).await;
    }
}
