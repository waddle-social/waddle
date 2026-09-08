use super::*;
use crate::ingress::{
    execute::{execute_effects, terminalize_if_complete},
    test_support::IngressFixture,
};
use crate::server::routes::interpret::{
    effects::{AuthorizationDeniedReason, ImmediateSink, PlanRejection},
    Deps,
};
use waddle_xmpp::ingress::{DmPinMutationAction, FrozenStanzaError, FrozenStanzaErrorType};

async fn accepted_alias_precedes_denial(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("accepted-unpin-policy-drift"), "unpin");
    let intent = IngressEffectIntent::DmPinMutation {
        // Recorded pin obligations carry the normalized (low, high) pair.
        pair: (
            "juliet@example.com".parse().expect("peer"),
            submission.sender.to_bare(),
        ),
        target_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
            "gone-target",
            submission.sender.to_bare().into(),
        ),
        action: DmPinMutationAction::Unpin,
    };
    submission.plan.intents.push(intent.clone());
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("accepted authority");
    let key = first.message_key.expect("key");
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("unreceipted mutation"));
    let error = FrozenStanzaError::new(
        FrozenStanzaErrorType::Cancel,
        waddle_xmpp::StanzaErrorCondition::Forbidden,
    );
    let mut reply = submission.plan.sanitized_message.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.payloads.push(error.to_xmpp().into());
    submission.plan.rejection = Some(PlanRejection::AuthorizationDenied(
        AuthorizationDeniedReason::BlockedSender,
    ));
    submission.plan.error_reply = Some(waddle_xmpp::Stanza::Message(reply));
    submission.plan.intents = vec![IngressEffectIntent::ErrorReply {
        recipient: submission.sender.clone(),
        error,
    }];
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("alias retry honors acceptance");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(replay.alias, AliasOutcomeClass::Existing);
    assert!(replay.class.advances());
    assert_ne!(replay.class, IngressDecisionClass::AuthorizationDenied);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(
        replay.external.len(),
        1,
        "recorded mutation reconstructed despite denial"
    );
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.web_socket_state = Some(state.as_ref());
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    let receipt = crate::ingress::durable::receipt_key(&intent).expect("receipt");
    let mut tx = fixture.uow.begin().await.expect("receipt read");
    assert!(crate::ingress_uow::EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("receipt landed"));
    tx.commit().await.expect("read commit");
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("repaired terminal"));
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_accepted_alias_denial_retry_repairs_recorded_unpin() {
    accepted_alias_precedes_denial(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_accepted_alias_denial_retry_repairs_recorded_unpin() {
    if let Some(fixture) = IngressFixture::postgres("accepted_alias_denial").await {
        accepted_alias_precedes_denial(fixture).await;
    }
}

/// RFC 0018 §3: a committed denial owns its origin id. A retransmission on a
/// new wire position after the account is created must re-emit the recorded
/// bounce, never plan the delivery today's policy would now allow.
async fn nonexistent_account_bounce_replay(fixture: IngressFixture) {
    use crate::ingress::IngressStreamIdentity;
    use crate::ingress_uow::SmIngressStreamRepository;
    use crate::server::routes::interpret::effects::ExternalEffect;
    use waddle_xmpp::ingress::{DigestContext, DigestInput, WireHandledCount};

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
    let mut submission = fixture.submission(Some("ghost-bounce"), "hello");
    let ghost: jid::BareJid = format!("ghost@{}", state.deps.auth_state.xmpp_domain)
        .parse()
        .expect("absent recipient");
    submission.plan.sanitized_message.to = Some(ghost.clone().into());
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(ghost.clone());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let sibling = submission
        .sender
        .to_bare()
        .with_resource_str("sibling")
        .expect("sibling");
    let (sibling_tx, mut sibling_rx) = tokio::sync::mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(sibling.clone(), sibling_tx);
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_carbons_enabled(&sibling, true));
    let incoming = submission.plan.sanitized_message.clone();
    let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new("ghost-bounce-stream");
    let mut tx = fixture.uow.begin().await.expect("begin");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("commit stream");
    let wire_identity = |position: u32| IngressStreamIdentity::Resumable {
        stream_id: stream_id.clone(),
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(position),
        checkpoint_h: WireHandledCount::new(position),
    };
    submission.identity = wire_identity(1);
    submission.plan = plan_dm(&state, &deps, submission.sender.clone(), incoming.clone()).await;
    assert!(
        matches!(
            submission.plan.rejection,
            Some(
                crate::server::routes::interpret::effects::PlanRejection::PolicyDenied(
                    crate::server::routes::interpret::effects::PolicyDeniedReason::StanzaError(_)
                )
            )
        ),
        "absent account bounces as a committed policy denial: {:?}",
        submission.plan.rejection
    );
    let denied = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("committed denial");
    assert_eq!(denied.class, IngressDecisionClass::PolicyDenied);
    assert!(denied.class.advances());
    let key = denied.message_key.expect("canonical key");
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 0);
    assert_eq!(fixture.count("inbox_entries").await, 0);
    let recorded_reply = denied
        .external
        .iter()
        .filter(|effect| matches!(effect, ExternalEffect::Frame(_)))
        .count();
    assert_eq!(recorded_reply, 1, "the denial owns exactly one reply frame");

    let mut first_report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &denied,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(first_report.receipt_failures.is_empty());
    assert!(
        sibling_rx.try_recv().is_ok(),
        "sent carbon survives the committed bounce"
    );
    assert!(sibling_rx.try_recv().is_err());
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        1,
        "only the carbon is complete before the error frame write"
    );
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("reply pending"));
    assert_eq!(first_report.frame_obligations.len(), 1);
    assert!(first_report
        .complete_frame_obligations(&fixture.uow, &fixture.db, std::time::Duration::from_secs(5))
        .await
        .expect("write bounce receipt"));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);

    // The account now exists, so current policy plans a full delivery.
    crate::server::routes::websocket::tests::seed_local_account(&state, "ghost").await;
    submission.identity = wire_identity(2);
    submission.plan = plan_dm(&state, &deps, submission.sender.clone(), incoming.clone()).await;
    assert_eq!(submission.plan.rejection, None);
    assert!(submission.plan.intents.iter().any(|intent| matches!(intent,
        waddle_xmpp::ingress::IngressEffectIntent::ArchiveAuthoritative { archive, .. }
            if archive == &ghost)));

    let replay = commit_submission(&fixture.uow, &submission, 2)
        .await
        .expect("recorded denial replay");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(replay.alias, AliasOutcomeClass::Existing);
    assert_eq!(replay.class, IngressDecisionClass::ExistingCommitted);
    assert!(replay.class.advances());
    assert_eq!(
        replay
            .external
            .iter()
            .filter(|effect| matches!(effect, ExternalEffect::Frame(_)))
            .count(),
        1,
        "the recorded bounce is reconstructed"
    );
    assert!(
        replay
            .external
            .iter()
            .all(|effect| matches!(effect, ExternalEffect::Frame(_) | ExternalEffect::Delivery(crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect::Carbons { .. }))),
        "no recipient delivery is invented: {:?}",
        replay.external
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 0);
    assert_eq!(fixture.count("inbox_entries").await, 0);
    let intents = fixture.count("ingress_effect_intents").await;
    assert_eq!(
        intents, 2,
        "recorded error reply and sender carbon remain obligations"
    );
    let mut replay_report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(replay_report.receipt_failures.is_empty());
    assert!(
        sibling_rx.try_recv().is_err(),
        "receipted carbon is not duplicated"
    );
    assert_eq!(
        replay_report.frame_obligations.len(),
        1,
        "recorded bounce is re-emitted"
    );
    assert!(replay_report
        .complete_frame_obligations(&fixture.uow, &fixture.db, std::time::Duration::from_secs(5))
        .await
        .expect("replay frame written"));
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        2,
        "receipts persist once"
    );
    fixture.close().await;
}

/// Plan the offered stanza through the live handler path for this connection.
async fn plan_dm(
    state: &crate::server::routes::websocket::WebSocketState,
    deps: &crate::server::routes::interpret::Deps<'_>,
    sender: jid::FullJid,
    incoming: xmpp_parsers::message::Message,
) -> crate::server::routes::interpret::effects::IngressPlan {
    let mut machine = waddle_xmpp::protocol::XmppStateMachine::new(
        "example.com",
        (*state.deps.protocol.dispatcher).clone(),
    );
    machine.transition_to_ready(sender, false);
    crate::server::routes::interpret::plan_message_dispatch(&mut machine, incoming, deps).await
}

#[tokio::test]
async fn sqlite_nonexistent_account_bounce_replay_keeps_recorded_denial() {
    nonexistent_account_bounce_replay(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_nonexistent_account_bounce_replay_keeps_recorded_denial() {
    if let Some(fixture) = IngressFixture::postgres("nonexistent_account_bounce").await {
        nonexistent_account_bounce_replay(fixture).await;
    }
}
