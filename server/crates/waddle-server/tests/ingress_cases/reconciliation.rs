use super::*;

async fn intent_repair_and_divergence(fixture: IngressFixture) {
    use waddle_xmpp::ingress::EffectMessageIdentity;
    let base = archive_plan(
        &fixture,
        Some("audience-origin"),
        "audience body",
        "audience-archive",
    );
    let first = commit_submission(&fixture.uow, &base, 5)
        .await
        .expect("initial archive");
    let mut repair = base.clone();
    repair.plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("phone")],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    });
    let repaired = commit_submission(&fixture.uow, &repair, 5)
        .await
        .expect("missing intent repair");
    assert_eq!(repaired.class, IngressDecisionClass::ExistingRepaired);
    assert_eq!(repaired.message_key, first.message_key);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    let mut divergent = repair;
    for intent in &mut divergent.plan.intents {
        if let IngressEffectIntent::RouteDirect { fanout, .. } = intent {
            *fanout = vec!["juliet@example.com/laptop".parse().expect("laptop")];
        }
    }
    let decision = commit_submission(&fixture.uow, &divergent, 5)
        .await
        .expect("audience drift keeps recorded intent");
    assert_eq!(decision.class, IngressDecisionClass::ExistingDivergent);
    assert_eq!(decision.message_key, first.message_key);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("inspect recorded audience");
    let recorded = waddle_server::ingress_uow::EffectIntentRepository::load(
        &mut tx,
        first.message_key.expect("key"),
    )
    .await
    .expect("recorded intents");
    assert!(recorded.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { fanout, .. } if fanout == &vec!["juliet@example.com/phone".parse::<jid::FullJid>().expect("phone")])));
    tx.commit().await.expect("close read");
    fixture.close().await;
}
#[tokio::test]
async fn ingress_intent_repair_divergence_sqlite() {
    intent_repair_and_divergence(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_intent_repair_divergence_postgres() {
    if let Some(fixture) = IngressFixture::postgres("intent_repair_divergence").await {
        intent_repair_and_divergence(fixture).await;
    }
}

async fn contradictory_intents(fixture: IngressFixture) {
    let mut submission = archive_plan(
        &fixture,
        Some("contradiction-origin"),
        "never committed",
        "first-identity",
    );
    let mut contradiction = submission.plan.intents[0].clone();
    if let IngressEffectIntent::ArchiveAuthoritative { stanza_id, .. } = &mut contradiction {
        stanza_id.id = "second-identity".to_owned();
    }
    submission.plan.intents.push(contradiction);
    let failure = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect_err("two identities for one authority");
    assert_eq!(failure.class(), IngressDecisionClass::IntentContradiction);
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(
            fixture.count(table).await,
            0,
            "contradiction rollback {table}"
        );
    }
    fixture.close().await;
}
#[tokio::test]
async fn ingress_intent_contradiction_sqlite() {
    contradictory_intents(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_intent_contradiction_postgres() {
    if let Some(fixture) = IngressFixture::postgres("intent_contradiction").await {
        contradictory_intents(fixture).await;
    }
}

async fn fresh_archive_conflict_is_storage(fixture: IngressFixture) {
    let first = commit_submission(
        &fixture.uow,
        &archive_plan(&fixture, None, "first", "colliding-archive-id"),
        5,
    )
    .await
    .expect("initial archive");
    let failure = commit_submission(
        &fixture.uow,
        &archive_plan(
            &fixture,
            None,
            "different offered message",
            "colliding-archive-id",
        ),
        5,
    )
    .await
    .expect_err("fresh primary-key collision");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    assert!(first.message_key.is_some());
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(
            fixture.count(table).await,
            1,
            "fresh collision rollback {table}"
        );
    }
    assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_fresh_archive_conflict_sqlite() {
    fresh_archive_conflict_is_storage(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_fresh_archive_conflict_postgres() {
    if let Some(fixture) = IngressFixture::postgres("fresh_archive_conflict").await {
        fresh_archive_conflict_is_storage(fixture).await;
    }
}
