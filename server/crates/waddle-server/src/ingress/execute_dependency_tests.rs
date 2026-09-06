use super::*;
use crate::{
    config::LineageConfig,
    db::{DatabaseConfig, DatabaseDriver},
    ingress::decision::{AliasOutcomeClass, IngressDecisionClass},
    server::routes::interpret::effects::{
        early::RoomMembershipMutation, invite::InviteDeliveryFailure, PlanEffectDependency,
    },
    server::routes::websocket::{
        handlers::message::muc_invite::{InviteLedgerMutation, MucMembershipMutation},
        muc_invites::OutstandingInvite,
    },
};
use kameo::actor::Spawn;
use waddle_xmpp::{
    muc::{
        room_actor::{GetSnapshot, RoomActor},
        MucRoom, RoomConfig,
    },
    xep::xep0421::OccupantIdSecret,
};

async fn database() -> (Database, IngressUnitOfWork) {
    let db = Database::from_config(
        "phase-c-dependencies",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, ":memory:"),
    )
    .await
    .expect("database");
    let uow = IngressUnitOfWork::open(
        db.clone(),
        LineageConfig {
            deployment_uuid: Some(crate::db::lineage::DeploymentUuid(uuid::Uuid::new_v4())),
            action: None,
        },
    )
    .expect("uow");
    (db, uow)
}

fn decision(
    external: Vec<ExternalEffect>,
    external_dependencies: Vec<Vec<PlanEffectDependency>>,
) -> IngressDecision {
    let count = external.len();
    IngressDecision {
        class: IngressDecisionClass::Accepted,
        message_key: None,
        ordinal: None,
        alias: AliasOutcomeClass::NoOrigin,
        verdict: None,
        archive_ids: vec![],
        applied_durable: Default::default(),
        external,
        external_dependencies,
        external_receipts: vec![vec![]; count],
        receipts_pending: vec![],
    }
}

#[tokio::test]
async fn membership_runs_before_dependents_and_preserved_grant_cannot_be_compensated() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    // Force ledger failure after successful membership resolution.
    state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database guard")
        .execute("DROP TABLE IF EXISTS muc_pending_invites", ())
        .await
        .expect("remove ledger");
    let invite = OutstandingInvite {
        room: "room@muc.example.com".parse().expect("room"),
        invitee: "member@example.com".parse().expect("member"),
        inviter: "sender@example.com".parse().expect("sender"),
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
    room.set_affiliation(invite.invitee.clone(), waddle_xmpp::Affiliation::Member);
    let actor = RoomActor::spawn(RoomActor::new(
        room,
        OccupantIdSecret::new(vec![b'x'; 32]).expect("secret"),
    ));
    let grant = MucMembershipMutation {
        room: invite.room.clone(),
        invitee: invite.invitee.clone(),
        actor: actor.clone(),
        previous_affiliation: waddle_xmpp::Affiliation::None,
    };
    let membership = PlanEffectDependency::AfterRoomMembership {
        room: invite.room.clone(),
        member: invite.invitee.clone(),
    };
    let effects = vec![
        ExternalEffect::InviteLedger(InviteLedgerMutation::Record {
            invite: invite.clone(),
            recorded_at: chrono::Utc::now(),
            failure: Some(Box::new(InviteDeliveryFailure::RollbackMucMembership(
                Box::new(grant.clone()),
            ))),
        }),
        ExternalEffect::Frame(Box::new(Stanza::Message(
            xmpp_parsers::message::Message::new(None),
        ))),
        ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::Muc(Box::new(grant))),
    ];
    let decision = decision(
        effects,
        vec![vec![membership.clone()], vec![membership], vec![]],
    );
    let (db, uow) = database().await;
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
        state.as_ref(),
        None,
    );
    let report = execute_effects(
        &uow,
        &db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        report
            .outcomes
            .iter()
            .map(|(_, outcome)| *outcome)
            .collect::<Vec<_>>(),
        vec![
            ExternalOutcome::Failed,
            ExternalOutcome::Done,
            ExternalOutcome::Done
        ]
    );
    assert_eq!(
        report.frames.len(),
        1,
        "dependency executes even when placed after its dependent"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("room snapshot");
    assert_eq!(
        snapshot.room.get_affiliation(&invite.invitee),
        waddle_xmpp::Affiliation::Member,
        "ledger compensation must not revoke preserved membership"
    );
    actor.kill();
}

#[tokio::test]
async fn failed_dependency_withholds_and_meters_each_dependent() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let pair = crate::server::routes::websocket::DmPairKey::new(
        "first@example.com".parse().expect("first"),
        "second@example.com".parse().expect("second"),
    );
    let target = waddle_xmpp_core::xep0359::StanzaId::new("pin", pair.low_peer.clone().into());
    let effects = vec![
        ExternalEffect::Frame(Box::new(Stanza::Message(
            xmpp_parsers::message::Message::new(None),
        ))),
        ExternalEffect::DmPinMutation(
            crate::server::routes::websocket::handlers::message::dm_pin::DmPinMutation {
                pair: pair.clone(),
                target_stanza_id: target.clone(),
                action: waddle_xmpp::ingress::DmPinMutationAction::Unpin,
            },
        ),
    ];
    let decision = decision(
        effects,
        vec![
            vec![PlanEffectDependency::AfterDmPinMutation { pair, target }],
            vec![],
        ],
    );
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let (db, uow) = database().await;
    let report = execute_effects(
        &uow,
        &db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.frames.is_empty());
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Failed));
    assert!(metrics
        .counter_sum("ingress.effects.unresolved", &[("kind", "frame")])
        .is_some_and(|value| value >= 1));
    assert!(metrics
        .counter_sum("ingress.effects.unresolved", &[("kind", "direct")])
        .is_some_and(|value| value >= 1));
}

#[test]
fn invite_receipts_require_the_exact_captured_route_and_actual_delivery_proof() {
    use crate::server::routes::interpret::effects::invite::{MucUserDeliveryProof, MucUserRoute};
    use waddle_xmpp::{
        ingress::{EffectMessageIdentity, IngressEffectIntent, PendingDeliveryMutation},
        pending_delivery::{PendingPayload, PendingRow, PendingRowId},
    };
    let recipient: jid::BareJid = "recipient@example.com".parse().expect("recipient");
    let resource: jid::FullJid = "recipient@example.com/device".parse().expect("resource");
    let message = Box::new(xmpp_parsers::message::Message::new(Some(
        recipient.clone().into(),
    )));
    let row_id = PendingRowId::fresh();
    let identity = EffectMessageIdentity::CaptureOrdinal(7);
    let route = MucUserRoute {
        route_identity: Some(identity.clone()),
        recipient: recipient.clone(),
        resources: vec![resource.clone()],
        message: message.clone(),
        fallback: PendingRow {
            id: row_id.clone(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(message),
            flushed_in_session: None,
            outbound_sequence: None,
        },
        failure: None,
    };
    let effect = ExternalEffect::RouteToPeer(route);
    let route_intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.clone(),
        fanout: vec![resource.clone()],
        route_identity: identity,
    };
    let pending_intent = IngressEffectIntent::PendingDelivery {
        mutation: PendingDeliveryMutation::Transient {
            recipient: recipient.clone(),
            row_id: row_id.clone(),
        },
    };
    let wrong_route = IngressEffectIntent::RouteDirect {
        recipient,
        fanout: vec![resource.clone()],
        route_identity: EffectMessageIdentity::CaptureOrdinal(8),
    };
    let mapped = super::super::receipts::external_receipts(
        std::slice::from_ref(&effect),
        &[route_intent.clone(), pending_intent.clone(), wrong_route],
    )
    .expect("receipt mapping");
    let route_key = super::super::durable::receipt_key(&route_intent).expect("route key");
    let pending_key = super::super::durable::receipt_key(&pending_intent).expect("pending key");
    assert_eq!(mapped, vec![vec![route_key.clone(), pending_key.clone()]]);
    assert_eq!(
        proven_receipts(
            &effect,
            &EffectOutcome::MucUserDelivery(Ok(MucUserDeliveryProof::Delivered {
                resources: vec![resource]
            })),
            &mapped[0]
        ),
        vec![route_key]
    );
    assert_eq!(
        proven_receipts(
            &effect,
            &EffectOutcome::MucUserDelivery(Ok(MucUserDeliveryProof::Queued { row_id })),
            &mapped[0]
        ),
        vec![pending_key]
    );
}
