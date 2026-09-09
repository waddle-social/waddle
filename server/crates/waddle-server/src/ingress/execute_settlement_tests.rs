use super::*;
use crate::{
    ingress::decision::{AliasOutcomeClass, IngressDecisionClass},
    server::routes::interpret::effects::SettledOutcome,
};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};

fn route_intent(ordinal: u64) -> IngressEffectIntent {
    IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("resource")],
        route_identity: EffectMessageIdentity::CaptureOrdinal(ordinal),
    }
}

#[test]
fn settled_outcome_proves_only_persisted_keys_independently_of_completion() {
    let persisted = route_intent(1);
    let missing = route_intent(2);
    let persisted_key = crate::ingress::receipt_key(&persisted).expect("persisted key");
    let candidates = vec![
        persisted_key.clone(),
        crate::ingress::receipt_key(&missing).expect("missing key"),
    ];
    let effect = ExternalEffect::Frame(Box::new(Stanza::Message(
        xmpp_parsers::message::Message::new(None),
    )));
    for (completion, expected) in [
        (SettledCompletion::Complete, ExternalOutcome::Done),
        (SettledCompletion::Incomplete, ExternalOutcome::Failed),
        (SettledCompletion::Uncertain, ExternalOutcome::Uncertain),
    ] {
        let outcome = EffectOutcome::Settled(SettledOutcome {
            persisted: vec![persisted.clone()],
            completion,
            detached: Some(vec![(
                "juliet@example.com/phone".parse().expect("resource"),
                FullJidDeliveryOutcome::Delivered,
            )]),
        });
        assert_eq!(
            proven_receipts(&effect, &outcome, &candidates),
            vec![persisted_key.clone()]
        );
        assert_eq!(
            classify_outcome(&effect, outcome, &mut Vec::new()),
            expected
        );
    }
}

#[test]
fn generic_completion_cannot_settle_an_arm_owned_key() {
    let key = crate::ingress::receipt_key(&route_intent(1)).expect("receipt key");
    let effect = ExternalEffect::Frame(Box::new(Stanza::Message(
        xmpp_parsers::message::Message::new(None),
    )));
    let decision = IngressDecision {
        class: IngressDecisionClass::Accepted,
        message_key: None,
        ordinal: None,
        alias: AliasOutcomeClass::NoOrigin,
        verdict: None,
        archive_ids: Vec::new(),
        applied_durable: Default::default(),
        external: vec![effect.clone()],
        external_dependencies: vec![Vec::new()],
        external_receipts: vec![vec![key.clone()]],
        arm_owned_receipts: vec![key.clone()],
        route_progress: Vec::new(),
        receipts_pending: vec![key],
    };
    assert!(completed_receipts(
        &decision,
        &[(effect, ExternalOutcome::Done)],
        &decision.external_receipts,
        0
    )
    .is_empty());
}
