use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::effects::{invite::MucUserRoute, EffectSink, PlanSink};
use crate::server::routes::interpret::DeliveryExecutionContext;
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent, PendingDeliveryMutation},
    pending_delivery::{
        storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
    },
    protocol::CarbonKind,
    registry::ConnectionRegistry,
};

async fn invite_pending_storage(
    fixture: &IngressFixture,
    quota: QuotaPolicy,
) -> std::sync::Arc<dyn PendingDeliveryStorage> {
    std::sync::Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
            fixture.db.clone(),
            quota,
        )
        .await
        .expect("canonical pending storage"),
    )
}

fn invite_submission(
    fixture: &IngressFixture,
    resource: &jid::FullJid,
) -> crate::ingress::IngressSubmission {
    let recipient = resource.to_bare();
    let mut submission = fixture.submission(Some("online-invite"), "invitation");
    let message = Box::new(submission.plan.sanitized_message.clone());
    let row_id = PendingRowId::fresh();
    let identity = EffectMessageIdentity::CaptureOrdinal(0);
    submission.plan.intents = vec![
        IngressEffectIntent::RouteDirect {
            recipient: recipient.clone(),
            fanout: vec![resource.clone()],
            route_identity: identity.clone(),
        },
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: recipient.clone(),
                row_id: row_id.clone(),
            },
        },
    ];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::RouteToPeer(MucUserRoute {
            route_identity: Some(identity),
            recipient: recipient.clone(),
            resources: vec![resource.clone()],
            message: message.clone(),
            fallback: PendingRow {
                id: row_id,
                recipient: recipient.clone(),
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Transient(message),
                flushed_in_session: None,
                outbound_sequence: None,
            },
            failure: None,
        }),
    ))];
    submission
}

async fn invite_delivered(fixture: IngressFixture, live: bool) {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let resource = recipient.with_resource_str("phone").expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    if live {
        registry.register(resource.clone(), tx);
    }
    let submission = invite_submission(&fixture, &resource);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    assert_eq!(decision.external_receipts[0].len(), 2);
    assert!(
        decision.arm_owned_receipts.is_empty(),
        "specialized invitation delivery keeps generic settlement"
    );
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(rx.try_recv().is_ok(), live, "live invitation delivery");
    assert_eq!(
        fixture.count("pending_delivery").await,
        i64::from(!live),
        "an unavailable live resource uses the frozen pending fallback"
    );
    assert!(report.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert!(terminalize_if_complete(
        &fixture.uow,
        decision.message_key.expect("canonical message"),
        DeliveryExecutionContext::Live.into()
    )
    .await
    .expect("terminalize"));
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_online_invite_receipts_live_route_and_fallback() {
    invite_delivered(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_online_invite_receipts_live_route_and_fallback() {
    if let Some(fixture) = IngressFixture::postgres("online_invite_receipts").await {
        invite_delivered(fixture, true).await;
    }
}

#[tokio::test]
async fn sqlite_invite_offline_fallback_terminalizes_generically() {
    invite_delivered(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_invite_offline_fallback_terminalizes_generically() {
    if let Some(fixture) = IngressFixture::postgres("offline_invite_receipts").await {
        invite_delivered(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_invite_lost_receipts_retry_without_second_live_copy() {
    let fixture = IngressFixture::sqlite().await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    registry.register(target.clone(), tx);
    let decision = commit_submission(&fixture.uow, &invite_submission(&fixture, &target), 1)
        .await
        .expect("commit invitation");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    fixture.execute("CREATE TRIGGER fail_invite_receipt BEFORE INSERT ON ingress_effect_receipts BEGIN SELECT RAISE(ABORT, 'lost receipt'); END", ()).await;
    let first = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        rx.try_recv().is_ok(),
        "first invitation reached the live queue"
    );
    assert_eq!(
        first.outcomes[0].1,
        ExternalOutcome::Uncertain,
        "receipt failure injected"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture
        .execute("DROP TRIGGER fail_invite_receipt", ())
        .await;
    let repaired = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(repaired.outcomes[0].1, ExternalOutcome::Done);
    assert!(
        rx.try_recv().is_err(),
        "a lost receipt must not repeat the invitation"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_invite_concurrent_executions_accept_one_live_copy() {
    let fixture = IngressFixture::sqlite().await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    registry.register(target.clone(), tx);
    let decision = commit_submission(&fixture.uow, &invite_submission(&fixture, &target), 1)
        .await
        .expect("commit invitation");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    let (first, second) = tokio::join!(
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5)
        ),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5)
        ),
    );
    assert!(
        first.outcomes[0].1 == ExternalOutcome::Done
            || second.outcomes[0].1 == ExternalOutcome::Done
    );
    assert!(rx.try_recv().is_ok(), "one invitation accepted");
    assert!(
        rx.try_recv().is_err(),
        "concurrent execution must not enqueue another copy"
    );
    assert_eq!(fixture.count("pending_delivery").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_invite_partial_live_fanout_retries_only_missing_resource() {
    let fixture = IngressFixture::sqlite().await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("first target");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("second target");
    let (tx, mut first_rx) = tokio::sync::mpsc::channel(4);
    registry.register(first.clone(), tx);
    let mut submission = invite_submission(&fixture, &first);
    for intent in &mut submission.plan.intents {
        if let IngressEffectIntent::RouteDirect { fanout, .. } = intent {
            fanout.push(second.clone());
        }
    }
    let Effect::External(ExternalEffect::RouteToPeer(route)) = &mut submission.plan.plan[0].effect
    else {
        panic!("invitation route");
    };
    route.resources.push(second.clone());
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit invitation");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    let partial = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_ne!(partial.outcomes[0].1, ExternalOutcome::Done);
    assert!(
        first_rx.try_recv().is_ok(),
        "connected resource accepted its copy"
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "partial live acceptance must not create a duplicate offline copy"
    );
    let (tx, mut second_rx) = tokio::sync::mpsc::channel(4);
    registry.register(second, tx);
    let repaired = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(repaired.outcomes[0].1, ExternalOutcome::Done);
    assert!(
        second_rx.try_recv().is_ok(),
        "missing resource receives its copy"
    );
    assert!(
        first_rx.try_recv().is_err(),
        "accepted sibling must not receive the invitation again"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_invite_uncertain_live_send_falls_back_only_after_its_deadline() {
    let fixture = IngressFixture::sqlite().await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    registry.register(target.clone(), tx);
    let decision = commit_submission(&fixture.uow, &invite_submission(&fixture, &target), 1)
        .await
        .expect("commit invitation");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    fixture.execute("CREATE TRIGGER fail_invite_completion BEFORE UPDATE ON ingress_send_attempts WHEN NEW.state = 2 BEGIN SELECT RAISE(ABORT, 'lost completion'); END", ()).await;
    let first = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        rx.try_recv().is_ok(),
        "live copy was accepted before completion failed"
    );
    assert_eq!(first.outcomes[0].1, ExternalOutcome::Uncertain);
    registry.unregister(&target);
    let waiting = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(waiting.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    let retry = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(retry.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        fixture.count("pending_delivery").await,
        1,
        "expired unknown acceptance hands off to durable offline custody"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

async fn invite_quota_preserves_authorization_until_delivery(fixture: IngressFixture) {
    use crate::server::routes::{
        interpret::effects::invite::InviteDeliveryFailure,
        websocket::muc_invites::{self, OutstandingInvite},
    };
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::CountCap { max_rows: 0 }).await;
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let invite = OutstandingInvite {
        room: "room@conference.example.com".parse().expect("room"),
        invitee: target.to_bare(),
        inviter: fixture.principal.bare_jid().clone(),
    };
    let ledger = state.deps.app_state.db_pool.global_actor().clone();
    muc_invites::record_invite_at(ledger.clone(), &invite, chrono::Utc::now())
        .await
        .expect("record outstanding invitation");
    let mut submission = invite_submission(&fixture, &target);
    let Effect::External(ExternalEffect::RouteToPeer(route)) = &mut submission.plan.plan[0].effect
    else {
        panic!("invitation route");
    };
    route.failure = Some(Box::new(InviteDeliveryFailure::RemoveLedger(
        invite.clone(),
    )));
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit invitation");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    let quota = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(quota.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    assert_eq!(
        muc_invites::list_invites(ledger.clone(), &invite.room, &invite.invitee)
            .await
            .expect("preserved ledger")
            .len(),
        1
    );

    // Quota relief permits delivery, but an interrupted receipt transaction
    // must preserve both the authorization and the original retry obligation.
    let unlimited = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    deps.pending_delivery_storage = Some(&unlimited);
    let postgres = fixture.db.driver() == crate::db::DatabaseDriver::Postgres;
    if postgres {
        fixture.execute("CREATE FUNCTION fail_invite_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'lost receipt'; END $$", ()).await;
        fixture.execute("CREATE TRIGGER fail_invite_receipt BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION fail_invite_receipt()", ()).await;
    } else {
        fixture.execute("CREATE TRIGGER fail_invite_receipt BEFORE INSERT ON ingress_effect_receipts BEGIN SELECT RAISE(ABORT, 'lost receipt'); END", ()).await;
    }
    let failed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(failed.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        muc_invites::list_invites(ledger.clone(), &invite.room, &invite.invitee)
            .await
            .expect("preserved ledger after failed commit")
            .len(),
        1
    );
    fixture
        .execute(
            if postgres {
                "DROP TRIGGER fail_invite_receipt ON ingress_effect_receipts"
            } else {
                "DROP TRIGGER fail_invite_receipt"
            },
            (),
        )
        .await;
    let recovered = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(recovered.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(
        muc_invites::list_invites(ledger, &invite.room, &invite.invitee)
            .await
            .expect("delivered invitation remains declinable")
            .len(),
        1
    );
    let pending = unlimited
        .list(&target.to_bare())
        .await
        .expect("pending invitation");
    assert_eq!(pending.len(), 1);
    assert_eq!(
        unlimited
            .delete_row(&pending[0].id)
            .await
            .expect("consume invitation"),
        1
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    registry.register(target, tx);
    let stale = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(stale.outcomes[0].1, ExternalOutcome::Done);
    assert!(
        rx.try_recv().is_err(),
        "settled invitation cannot be sent again after pending consumption"
    );
    assert_eq!(fixture.count("pending_delivery").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_invite_quota_preserves_authorization_until_delivery() {
    invite_quota_preserves_authorization_until_delivery(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_invite_quota_preserves_authorization_until_delivery() {
    if let Some(fixture) = IngressFixture::postgres("invite_quota_compensation").await {
        invite_quota_preserves_authorization_until_delivery(fixture).await;
    }
}

async fn invite_live_acceptance_after_offline_probe_prevents_pending_fallback(
    fixture: IngressFixture,
) {
    use std::sync::Arc;
    use tokio::sync::Notify;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let decision = commit_submission(&fixture.uow, &invite_submission(&fixture, &target), 1)
        .await
        .expect("commit invitation");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let offline_attempt = crate::ingress::execute_uow::PAUSE_BEFORE_INVITATION_SETTLEMENT.scope(
        (entered.clone(), resume.clone()),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        ),
    );
    let live_attempt = async {
        entered.notified().await;
        registry.register(target, tx);
        let report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        resume.notify_one();
        report
    };
    let (offline, live) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(offline_attempt, live_attempt)
    })
    .await
    .expect("both competing invitation attempts finish");
    assert_eq!(live.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(offline.outcomes[0].1, ExternalOutcome::Done);
    assert!(
        rx.try_recv().is_ok(),
        "the live attempt accepted one invitation"
    );
    assert!(
        rx.try_recv().is_err(),
        "offline settlement must not repeat the live copy"
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "a stale offline probe must recheck live acceptance before allocating pending delivery"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_invite_live_acceptance_after_offline_probe_prevents_pending_fallback() {
    invite_live_acceptance_after_offline_probe_prevents_pending_fallback(
        IngressFixture::sqlite().await,
    )
    .await;
}

#[tokio::test]
async fn postgres_invite_live_acceptance_after_offline_probe_prevents_pending_fallback() {
    if let Some(fixture) = IngressFixture::postgres("invite_live_pending_race").await {
        invite_live_acceptance_after_offline_probe_prevents_pending_fallback(fixture).await;
    }
}

async fn invite_quota_recovers_generated_notification(
    fixture: IngressFixture,
    affiliation: waddle_xmpp::Affiliation,
) {
    use crate::server::routes::{
        interpret::effects::{
            early::RoomMembershipMutation, invite::InviteDeliveryFailure, PlanEffectDependency,
        },
        websocket::{
            handlers::message::muc_invite::{InviteLedgerMutation, MucMembershipMutation},
            muc_invites::OutstandingInvite,
        },
    };
    use kameo::actor::Spawn;
    use waddle_xmpp::{
        ingress::{MucInviteLedgerAction, MucInviteLedgerMutation, MucInviteMembershipGrant},
        muc::{
            room_actor::{GetSnapshot, RoomActor},
            MucRoom, RoomConfig,
        },
        xep::xep0421::OccupantIdSecret,
        Affiliation,
    };
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::CountCap { max_rows: 0 }).await;
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let invite = OutstandingInvite {
        room: "room@conference.example.com".parse().expect("room"),
        invitee: target.to_bare(),
        inviter: fixture.principal.bare_jid().clone(),
    };
    let mut room = MucRoom::new(
        invite.room.clone(),
        "test".into(),
        "room".into(),
        RoomConfig {
            members_only: true,
            ..RoomConfig::default()
        },
    );
    room.set_affiliation(invite.invitee.clone(), affiliation);
    let actor = RoomActor::spawn(RoomActor::new(
        room,
        OccupantIdSecret::new(vec![b'x'; 32]).expect("secret"),
    ));
    let grant = MucMembershipMutation {
        room: invite.room.clone(),
        invitee: invite.invitee.clone(),
        actor: actor.clone(),
        // Planning observed a different affiliation from the execution-time room.
        previous_affiliation: Affiliation::Outcast,
    };
    let membership = PlanEffectDependency::AfterRoomMembership {
        room: invite.room.clone(),
        member: invite.invitee.clone(),
    };
    let recorded_at = chrono::Utc::now();
    let mut submission = invite_submission(&fixture, &target);
    let inbound_invite =
        minidom::Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                invite.invitee.to_string(),
            )
            .build();
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(invite.room.clone());
    let incoming = &mut submission.plan.sanitized_message;
    incoming.to = Some(invite.room.clone().into());
    incoming.type_ = xmpp_parsers::message::MessageType::Normal;
    incoming.bodies.clear();
    incoming.payloads.push(
        minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(inbound_invite.clone())
            .build(),
    );
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        incoming,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![invite.inviter.clone()],
            stanza_lang: None,
        },
    )
    .expect("mediated invitation digest");
    let outgoing =
        crate::server::routes::websocket::handlers::message::muc_invite::mediated_invite_message(
            incoming,
            &invite.room,
            &invite.inviter,
            &invite.invitee,
            &inbound_invite,
        );
    let Effect::External(ExternalEffect::RouteToPeer(route)) = &mut submission.plan.plan[0].effect
    else {
        panic!("invitation route");
    };
    *route.message = outgoing.clone();
    route.fallback.payload = PendingPayload::Transient(Box::new(outgoing));
    route.failure = Some(Box::new(InviteDeliveryFailure::RollbackMucMembership(
        Box::new(grant.clone()),
    )));
    submission.plan.plan[0].dependencies = vec![
        membership.clone(),
        PlanEffectDependency::AfterInviteLedger {
            invite: invite.clone(),
        },
    ];
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::Muc(Box::new(grant))),
        )));
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::InviteLedger(
            InviteLedgerMutation::Record {
                invite: invite.clone(),
                recorded_at,
                failure: None,
            },
        )))
        .with_dependency(membership),
    );
    submission.plan.intents.extend([
        IngressEffectIntent::MucInviteMembershipGrant {
            grant: MucInviteMembershipGrant {
                room: invite.room.clone(),
                invitee: invite.invitee.clone(),
                inviter: invite.inviter.clone(),
            },
        },
        IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: invite.room.clone(),
                invitee: invite.invitee.clone(),
                inviter: invite.inviter.clone(),
                action: MucInviteLedgerAction::Recorded,
                recorded_at: Some(recorded_at),
            },
        },
    ]);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit invitation grant");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        report.outcomes.iter().any(|(effect, outcome)| matches!(
            effect,
            ExternalEffect::RouteToPeer(_)
        ) && *outcome == ExternalOutcome::Uncertain),
        "quota exhaustion preserves the invitation for retry"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("room snapshot");
    assert_eq!(
        snapshot.room.get_affiliation(&invite.invitee),
        Affiliation::Member,
        "canonical quota exhaustion preserves both existing and newly granted membership"
    );
    assert_eq!(fixture.count("pending_delivery").await, 0);
    let key = decision.message_key.expect("canonical invitation");
    drop(decision);
    drop(submission);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("durable recovery snapshot");
    let recorded = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded intents");
    let envelope = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("canonical envelope");
    let receipts = EffectReceiptRepository::keys(&mut tx, key)
        .await
        .expect("durable prerequisites");
    let pending: Vec<_> = recorded
        .iter()
        .filter(|intent| !receipts.contains(&crate::ingress::receipt_key(intent).expect("receipt")))
        .cloned()
        .collect();
    tx.commit().await.expect("snapshot committed");
    assert_eq!(pending.len(), 2, "only route and fallback need recovery");
    let rebuild = |recorded: &[IngressEffectIntent],
                   pending: &[IngressEffectIntent],
                   blocked: &[jid::BareJid]| {
        crate::ingress::recovery_rebuild::rebuild(crate::ingress::recovery_rebuild::RecoveryInput {
            key,
            envelope: &envelope,
            created_at: recorded_at,
            recorded,
            unreceipted: pending,
            route_progress: pending
                .iter()
                .filter_map(|intent| {
                    crate::ingress::recorded::RouteProgress::from_intent(intent, None, vec![])
                        .expect("progress")
                })
                .collect(),
            host_owned_resources: vec![],
            departed_occupants: vec![],
            blocked_recipients: blocked,
        })
        .expect("rebuild generated invitation")
    };
    let missing_prerequisite = rebuild(&recorded, &recorded, &[]);
    assert!(
        missing_prerequisite.decision.external.is_empty(),
        "recovery cannot grant membership or send before durable prerequisites"
    );
    let blocked = rebuild(&recorded, &pending, std::slice::from_ref(&invite.invitee));
    assert!(blocked.decision.external.is_empty());
    assert_eq!(
        blocked.discarded_receipts.len(),
        2,
        "blocking resolves both route and fallback without delivery"
    );
    let mut expired = recorded.clone();
    for intent in &mut expired {
        if let IngressEffectIntent::MucInviteLedger { mutation } = intent {
            mutation.recorded_at = Some(
                chrono::Utc::now()
                    - crate::server::routes::websocket::muc_invites::INVITE_TTL
                    - chrono::Duration::seconds(1),
            );
        }
    }
    let expired = rebuild(&expired, &pending, &[]);
    assert!(expired.decision.external.is_empty());
    assert_eq!(
        expired.discarded_receipts.len(),
        2,
        "expired notification resolves both delivery obligations"
    );
    let rebuilt = rebuild(&recorded, &pending, &[]);
    assert_eq!(rebuilt.decision.external.len(), 1);
    assert!(rebuilt.unsupported_receipts.is_empty());
    assert!(
        matches!(&rebuilt.decision.external[0], ExternalEffect::RouteToPeer(route) if route.failure.is_none()),
        "recovery reconstructs notification only, without compensation or grants"
    );
    let unlimited = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    deps.pending_delivery_storage = Some(&unlimited);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let recovered = execute_effects(
        &fixture.uow,
        &fixture.db,
        &rebuilt.decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(recovered.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 4);
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("membership preserved")
            .room
            .get_affiliation(&invite.invitee),
        Affiliation::Member
    );
    actor.kill();
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_invite_quota_recovers_generated_notification_after_restart() {
    for affiliation in [
        waddle_xmpp::Affiliation::Member,
        waddle_xmpp::Affiliation::None,
    ] {
        invite_quota_recovers_generated_notification(IngressFixture::sqlite().await, affiliation)
            .await;
    }
}

#[tokio::test]
async fn postgres_invite_quota_recovers_generated_notification_after_restart() {
    if let Some(fixture) = IngressFixture::postgres("invite_quota_restart").await {
        invite_quota_recovers_generated_notification(fixture, waddle_xmpp::Affiliation::None).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn sqlite_invite_remote_hosted_resource_uses_gateway_without_pending_fallback() {
    use crate::server::routes::interpret::CONTROLLED_REGISTERED_REMOTE_DELIVERY;
    use std::sync::{Arc, Mutex};
    let fixture = IngressFixture::sqlite().await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/remote".parse().expect("remote target");
    let (tx, mut mirror) = tokio::sync::mpsc::channel(4);
    registry.register_entry(
        target.clone(),
        waddle_xmpp::registry::ConnectionEntry::remote_hosted(tx),
    );
    let submission = invite_submission(&fixture, &target);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("record invitation");
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let report = CONTROLLED_REGISTERED_REMOTE_DELIVERY
        .scope(
            (FullJidDeliveryOutcome::MaybeCommitted, calls.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    assert_eq!(
        calls.lock().expect("remote calls").len(),
        1,
        "the remote-hosted invitation must reach its registered socket gateway"
    );
    assert_eq!(calls.lock().expect("remote calls")[0].0, target);
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert!(
        mirror.try_recv().is_err(),
        "the owner mirror is not a local socket"
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "an uncertain remote send cannot fall back to pending delivery"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

async fn remote_carbons_failure(fixture: IngressFixture) {
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let mut submission = fixture.submission(Some("remote-carbon"), "carbon body");
    let owner = submission.sender.to_bare();
    let exclude = vec![submission.sender.clone()];
    let intent = IngressEffectIntent::RelayCarbons {
        owner: owner.clone(),
        exclude: exclude.clone(),
        kind: CarbonKind::Sent,
    };
    let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        owner,
        exclude,
        kind: CarbonKind::Sent,
        origin: None,
        message: Box::new(submission.plan.sanitized_message.clone()),
    });
    let sink = PlanSink::new();
    sink.observe_sender(&submission.sender);
    sink.record(PlannedEffect::new(Effect::External(effect.clone())));
    submission.plan.plan = sink.take().0;
    submission.plan.intents = vec![intent];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Failed);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    let key = decision.message_key.expect("canonical key");
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending")
    );
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry");
    assert_eq!(retry.external.len(), 1, "unreceipted remote fanout retries");
    assert_eq!(retry.external_receipts[0].len(), 1);
    // The owner-reply proof is the same typed completion consumed by Phase C.
    let outcome = EffectOutcome::Delivery(FullJidDeliveryOutcome::Delivered);
    let proven = vec![proven_receipts(
        &effect,
        &outcome,
        &retry.external_receipts[0],
    )];
    let classified = classify_outcome(&effect, outcome, &mut Vec::new());
    let completed = completed_receipts(&retry, &[(effect, classified)], &proven, 0);
    assert_eq!(completed.len(), 1);
    for receipt in completed {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("owner reply receipt");
    }
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("terminalized")
    );
    let confirmed_retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("confirmed retry");
    let report = tokio::time::timeout(
        Duration::from_secs(2),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &confirmed_retry,
            &ImmediateSink,
            &deps,
            Duration::from_secs(1),
        ),
    )
    .await
    .expect("confirmed retry finishes");
    assert_eq!(
        report.outcomes[0].1,
        ExternalOutcome::Done,
        "confirmed remote delivery is not attempted against unavailable bridge"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_carbons_failure_retains_intent_and_retry_success_receipts() {
    remote_carbons_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_remote_carbons_failure_retains_intent_and_retry_success_receipts() {
    if let Some(fixture) = IngressFixture::postgres("remote_carbon_receipts").await {
        remote_carbons_failure(fixture).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn remote_carbons_planning_requires_owner_inventory_before_commit() {
    use crate::clustering::route_bridge::{
        OrderedRelayDeliveryBridge, RemoteResourceOriginSnapshot,
    };
    use crate::server::routes::interpret::{
        interpret, OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind,
    };
    use std::sync::Arc;
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                ordered_relay_delivery_bridge: Some(bridge),
                ..Default::default()
            },
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        )
        .await;
    let registry = ConnectionRegistry::new();
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("source");
    let owner = sender.to_bare();
    let sink = PlanSink::new();
    sink.observe_sender(&sender);
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::RemoteResource(RemoteResourceOriginSnapshot {
            jid: sender.clone(),
            registration_id: serde_json::from_value(serde_json::json!(uuid::Uuid::new_v4()))
                .expect("registration id"),
            socket_generation: serde_json::from_value(serde_json::json!(1))
                .expect("socket generation"),
            user_owner: crate::clustering::NodeId::new("remote-owner".into()),
        }),
        sender_entity: waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            owner.to_string(),
        ),
        inbound_sequence: 1,
        handoff: None,
    });
    let mut message =
        xmpp_parsers::message::Message::new(Some("juliet@example.com".parse().expect("recipient")));
    message.from = Some(sender.clone().into());
    interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::SendCarbons {
            owner: owner.clone(),
            message: Box::new(message),
            kind: CarbonKind::Sent,
            exclude: vec![sender.clone()],
        }],
        &deps,
    )
    .await;
    assert!(
        capture.snapshot().intents.is_empty(),
        "an unreachable inventory cannot freeze a guessed audience"
    );
    assert!(
        sink.take().0.is_empty(),
        "no post-commit relay may choose new carbon targets"
    );
    assert_eq!(
        sink.failure(),
        Some(crate::server::routes::interpret::effects::PlanFailure::CarbonInventoryRead)
    );
}

#[path = "pending_exclusion_tests.rs"]
mod pending_exclusion;

#[tokio::test]
async fn sqlite_invite_expired_partial_start_hands_off_without_repeating_live_sibling() {
    use crate::ingress_uow::{SendAttemptRepository, SendClaim, SendObligation};
    let fixture = IngressFixture::sqlite().await;
    let storage = invite_pending_storage(&fixture, QuotaPolicy::Unlimited).await;
    let registry = ConnectionRegistry::new();
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("first");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("second");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    registry.register(first.clone(), sender);
    let mut submission = invite_submission(&fixture, &first);
    let route_intent = submission
        .plan
        .intents
        .iter_mut()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteDirect { .. }))
        .expect("route");
    if let IngressEffectIntent::RouteDirect { fanout, .. } = route_intent {
        fanout.push(second.clone());
    }
    let receipt = crate::ingress::receipt_key(route_intent).expect("receipt");
    let Effect::External(ExternalEffect::RouteToPeer(route)) = &mut submission.plan.plan[0].effect
    else {
        panic!("invitation")
    };
    route.resources.push(second.clone());
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("canonical");
    let key = decision.message_key.expect("key");
    let mut tx = fixture.uow.begin().await.expect("start");
    let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
        &mut tx,
        &SendObligation {
            message: key,
            receipt,
            recipient: second,
        },
        &waddle_xmpp::ownership::NodeIdentity::local(),
        Duration::from_secs(5),
    )
    .await
    .expect("claim") else {
        panic!("fresh")
    };
    assert!(SendAttemptRepository::start(&mut tx, &lease)
        .await
        .expect("started"));
    tx.commit().await.expect("commit");
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    let before = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_ne!(before.outcomes[0].1, ExternalOutcome::Done);
    assert!(receiver.try_recv().is_ok());
    assert_eq!(fixture.count("pending_delivery").await, 0);
    fixture
        .execute(
            "UPDATE ingress_send_attempts SET expires_at_ms = 0 WHERE state = 1",
            (),
        )
        .await;
    let denied = invite_pending_storage(&fixture, QuotaPolicy::CountCap { max_rows: 0 }).await;
    deps.pending_delivery_storage = Some(&denied);
    let quota = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(quota.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        0,
        "quota cannot terminally refuse possibly delivered invitation"
    );
    deps.pending_delivery_storage = Some(&storage);
    let after = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(after.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(
        receiver.try_recv().is_err(),
        "accepted sibling is not directly sent twice"
    );
    fixture.execute("DELETE FROM pending_delivery", ()).await;
    let replay = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(replay.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    fixture.close().await;
}
