//! XEP-0313 §5.1.3: recipient dispatch and MAM expose the same archive order.
use std::{sync::Arc, time::Duration};

use jid::FullJid;
use waddle_server::ingress::{
    commit::commit_submission, Deps, IngressDecisionClass, IngressPrincipal, IngressStreamIdentity,
    IngressSubmission, RecoveryEnvironment,
};
use waddle_xmpp::{
    auth::{AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch},
    ingress::{DigestContext, DigestInput, NormalizedTarget},
    registry::ConnectionRegistry,
    stream_management::InMemorySmSessionRegistry,
};
use waddle_xmpp_core::xep0359::{add_stanza_id, extract_stanza_ids, StanzaId};

use crate::{detached_progress_support as delivery, ingress_support::IngressFixture};

async fn second_sender(fixture: &IngressFixture) -> AuthenticatedPrincipalRef {
    let principal = AuthenticatedPrincipalRef::new(
        "mercutio@example.com".parse().expect("second sender"),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::new(3),
        PrincipalAuthEpoch::new(5),
    );
    let now = chrono::Utc::now().to_rfc3339();
    fixture.execute(
        "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        waddle_server::db_params![principal.bare_jid().to_string(), "mercutio".to_owned(), "mercutio".to_owned(), now.clone(), now.clone()],
    ).await;
    fixture.execute(
        "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        waddle_server::db_params!["second-session".to_owned(), principal.bare_jid().to_string(), "second-token".to_owned(), principal.auth_context_id().as_uuid().to_string(), 3_i64, 5_i64, now.clone(), now],
    ).await;
    principal
}

fn submission(
    fixture: &IngressFixture,
    principal: &AuthenticatedPrincipalRef,
    id: &str,
    targets: &[FullJid],
    full_jid: bool,
) -> IngressSubmission {
    let mut submission = fixture.submission(Some(id), "archive body");
    submission.sender = principal
        .bare_jid()
        .with_resource_str("phone")
        .expect("sender");
    submission.principal = IngressPrincipal::Authenticated(principal.clone());
    submission.identity = IngressStreamIdentity::Ephemeral {
        principal: principal.clone(),
    };
    submission.plan.sanitized_message.from = Some(submission.sender.clone().into());
    submission.target = if full_jid {
        NormalizedTarget::Full(targets[0].clone())
    } else {
        NormalizedTarget::Bare(targets[0].to_bare())
    };
    submission.plan.sanitized_message.to = Some(if full_jid {
        targets[0].clone().into()
    } else {
        targets[0].to_bare().into()
    });
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("sender and target digest");
    let recipient = targets[0].to_bare();
    super::add_archive(
        &mut submission,
        principal.bare_jid(),
        &format!("sender-{id}"),
        chrono::Utc::now(),
    );
    super::add_archive(&mut submission, &recipient, id, chrono::Utc::now());
    add_stanza_id(
        &mut submission.plan.sanitized_message,
        &StanzaId::new(id, recipient.into()),
    );
    delivery::route(&mut submission, targets, 0);
    submission
}

async fn wire_ids(sm: &InMemorySmSessionRegistry, target: &FullJid) -> Vec<String> {
    delivery::queued(sm, target)
        .await
        .unacked_stanzas
        .iter()
        .map(|queued| {
            let xml: minidom::Element = queued.stanza_xml.parse().expect("persisted wire stanza");
            let message = xmpp_parsers::message::Message::try_from(xml).expect("wire message");
            extract_stanza_ids(&message)
                .into_iter()
                .find(|id| id.by == jid::Jid::from(target.to_bare()))
                .expect("recipient archive UID on wire")
                .id
        })
        .collect()
}

async fn assert_archive_matches_wire(
    fixture: &IngressFixture,
    sm: &InMemorySmSessionRegistry,
    target: &FullJid,
) {
    let archive = super::query_wire(fixture, &target.to_bare()).await;
    let ids: Vec<_> = archive.into_iter().map(|row| row.id).collect();
    assert_eq!(ids, ["dispatch-a", "dispatch-b"]);
    assert_eq!(
        wire_ids(sm, target).await,
        ids,
        "delivery order must equal MAM order"
    );
}

async fn reversed_execution(fixture: IngressFixture, mixed_targets: bool) {
    let sender_b = second_sender(&fixture).await;
    let [target, _, _] = delivery::resources();
    let connections = ConnectionRegistry::new();
    let sm = delivery::registry(&fixture).await;
    delivery::attach(&sm, &target).await;
    let targets = std::slice::from_ref(&target);
    let a = submission(&fixture, &fixture.principal, "dispatch-a", targets, false);
    let b = submission(&fixture, &sender_b, "dispatch-b", targets, mixed_targets);
    let first = commit_submission(&fixture.uow, &a, 5)
        .await
        .expect("commit A");
    let second = commit_submission(&fixture.uow, &b, 5)
        .await
        .expect("commit B");
    assert_eq!(first.class, IngressDecisionClass::Accepted);
    assert_eq!(second.class, IngressDecisionClass::Accepted);
    delivery::execute(&fixture, &second, &connections, &sm).await;
    assert!(
        wire_ids(&sm, &target).await.is_empty(),
        "B cannot pass A's committed recipient archive position"
    );
    delivery::execute(&fixture, &first, &connections, &sm).await;
    assert_eq!(wire_ids(&sm, &target).await, ["dispatch-a"]);
    let retry = delivery::retry_decision(&fixture, &b).await;
    delivery::execute(&fixture, &retry, &connections, &sm).await;
    assert_archive_matches_wire(&fixture, &sm, &target).await;
    // Duplicate execution cannot append either already delivered archive entry again.
    for original in [&a, &b] {
        let retry = delivery::retry_decision(&fixture, original).await;
        delivery::execute(&fixture, &retry, &connections, &sm).await;
    }
    assert_archive_matches_wire(&fixture, &sm, &target).await;
    drop(sm);
    fixture.close().await;
}

async fn partial_fanout_restart(fixture: IngressFixture) {
    let sender_b = second_sender(&fixture).await;
    let [phone, laptop, _] = delivery::resources();
    let connections = ConnectionRegistry::new();
    let sm = delivery::registry(&fixture).await;
    delivery::attach(&sm, &phone).await;
    let targets = [phone.clone(), laptop.clone()];
    let a = submission(&fixture, &fixture.principal, "dispatch-a", &targets, false);
    let b = submission(&fixture, &sender_b, "dispatch-b", &targets, false);
    let first = commit_submission(&fixture.uow, &a, 5)
        .await
        .expect("commit A");
    let second = commit_submission(&fixture.uow, &b, 5)
        .await
        .expect("commit B");
    delivery::execute(&fixture, &first, &connections, &sm).await;
    delivery::execute(&fixture, &second, &connections, &sm).await;
    assert_archive_matches_wire(&fixture, &sm, &phone).await;
    drop(sm);

    let sm = delivery::registry(&fixture).await;
    assert_eq!(sm.restore_from_persistence().await.expect("restart SM"), 1);
    delivery::attach(&sm, &laptop).await;
    let second_retry = delivery::retry_decision(&fixture, &b).await;
    delivery::execute(&fixture, &second_retry, &connections, &sm).await;
    assert!(
        wire_ids(&sm, &laptop).await.is_empty(),
        "restart must retain laptop's unfinished A barrier"
    );
    let first_retry = delivery::retry_decision(&fixture, &a).await;
    delivery::execute(&fixture, &first_retry, &connections, &sm).await;
    let second_retry = delivery::retry_decision(&fixture, &b).await;
    delivery::execute(&fixture, &second_retry, &connections, &sm).await;
    assert_archive_matches_wire(&fixture, &sm, &phone).await;
    assert_archive_matches_wire(&fixture, &sm, &laptop).await;
    drop(sm);
    fixture.close().await;
}

async fn reversed_live_execution(fixture: IngressFixture) {
    use kameo::actor::Spawn;
    use waddle_server::ingress::{
        effects::{
            delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
            Effect,
        },
        execute::execute_effects,
        ExternalEffect, ImmediateSink,
    };
    use waddle_xmpp::{
        registry::{RegisterUserResource, UserRegistryActor},
        Stanza,
    };

    let sender_b = second_sender(&fixture).await;
    let [target, _, _] = delivery::resources();
    let connections = ConnectionRegistry::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    connections.register_with_carbons(target.clone(), sender, false);
    let users = UserRegistryActor::spawn(UserRegistryActor::new());
    users
        .ask(RegisterUserResource {
            entry: connections.get_entry(&target).expect("live resource"),
            jid: target.clone(),
        })
        .await
        .expect("register recipient");
    let mut deps = Deps::new(&connections, "example.com");
    deps.user_registry = Some(&users);
    let targets = std::slice::from_ref(&target);
    let mut a = submission(&fixture, &fixture.principal, "dispatch-a", targets, false);
    let mut b = submission(&fixture, &sender_b, "dispatch-b", targets, true);
    for submission in [&mut a, &mut b] {
        let route = submission.plan.plan.last_mut().expect("recipient route");
        route.effect = Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(
                    waddle_xmpp::ingress::EffectMessageIdentity::capture_ordinal(0),
                ),
                jid: target.clone(),
                stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
                kind: PeerDeliveryKind::DirectFrame,
                call_setup: None,
            },
        ));
    }
    let first = commit_submission(&fixture.uow, &a, 5)
        .await
        .expect("commit live A");
    let second = commit_submission(&fixture.uow, &b, 5)
        .await
        .expect("commit live B");
    execute_effects(
        &fixture.uow,
        &fixture.db,
        &second,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(receiver.try_recv().is_err(), "live B must wait for A");
    execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    let retry = delivery::retry_decision(&fixture, &b).await;
    execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    let mut delivered = Vec::new();
    for _ in 0..2 {
        let Stanza::Message(message) = receiver.try_recv().expect("ordered live delivery").stanza
        else {
            panic!("live message");
        };
        delivered.push(
            extract_stanza_ids(&message)
                .into_iter()
                .find(|id| id.by == jid::Jid::from(target.to_bare()))
                .expect("recipient archive UID")
                .id,
        );
    }
    let archived: Vec<_> = super::query_wire(&fixture, &target.to_bare())
        .await
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(archived, ["dispatch-a", "dispatch-b"]);
    assert_eq!(delivered, archived);
    assert!(receiver.try_recv().is_err());
    fixture.close().await;
}

struct Recovery {
    connections: ConnectionRegistry,
    sm: Arc<InMemorySmSessionRegistry>,
}

impl RecoveryEnvironment for Recovery {
    fn recovery_deps(&self) -> Deps<'_> {
        let mut deps = Deps::new(&self.connections, "example.com");
        deps.sm_session_registry = Some(&self.sm);
        deps
    }
}

async fn recovery_order(fixture: IngressFixture) {
    let sender_b = second_sender(&fixture).await;
    let [target, _, _] = delivery::resources();
    let sm = delivery::registry(&fixture).await;
    delivery::attach(&sm, &target).await;
    let targets = std::slice::from_ref(&target);
    let a = submission(&fixture, &fixture.principal, "dispatch-a", targets, false);
    let b = submission(&fixture, &sender_b, "dispatch-b", targets, true);
    let first = commit_submission(&fixture.uow, &a, 5)
        .await
        .expect("commit A without execution");
    let second = commit_submission(&fixture.uow, &b, 5)
        .await
        .expect("commit B without execution");
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE message_key = ?::uuid",
        waddle_server::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE message_key = ?",
    };
    // Scan B first: recovery scheduling timestamps cannot override archive order.
    for (decision, seconds) in [(first, 120), (second, 180)] {
        fixture
            .execute(
                sql,
                waddle_server::db_params![
                    (chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339(),
                    decision
                        .message_key
                        .expect("committed message")
                        .to_storage()
                        .to_string()
                ],
            )
            .await;
    }
    let authority = fixture.authority().await;
    let recovery: Arc<dyn RecoveryEnvironment> = Arc::new(Recovery {
        connections: ConnectionRegistry::new(),
        sm: sm.clone(),
    });
    authority.bind_recovery_environment(Arc::downgrade(&recovery));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            authority.trigger_maintenance();
            if fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("restart recovery drains both committed messages");
    assert_archive_matches_wire(&fixture, &sm, &target).await;
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    drop(recovery);
    drop(authority);
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn reversed_dispatch_sqlite() {
    reversed_execution(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn reversed_dispatch_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ord_rev").await {
        reversed_execution(fixture, false).await;
    }
}
#[tokio::test]
async fn mixed_bare_full_dispatch_sqlite() {
    reversed_execution(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn mixed_bare_full_dispatch_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ord_mixed").await {
        reversed_execution(fixture, true).await;
    }
}
#[tokio::test]
async fn partial_fanout_restart_sqlite() {
    partial_fanout_restart(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn partial_fanout_restart_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ord_part").await {
        partial_fanout_restart(fixture).await;
    }
}
#[tokio::test]
async fn recovered_dispatch_sqlite() {
    recovery_order(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn recovered_dispatch_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ord_rec").await {
        recovery_order(fixture).await;
    }
}

#[tokio::test]
async fn reversed_live_dispatch_sqlite() {
    reversed_live_execution(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn reversed_live_dispatch_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ord_live").await {
        reversed_live_execution(fixture).await;
    }
}

/// A quiet SM client need not acknowledge one pending copy before the next
/// ordered copy (or a live successor) may enter that same stream.
async fn pending_stream_progress(fixture: IngressFixture) {
    use kameo::actor::Spawn;
    use waddle_server::{
        ingress::{
            effects::{
                delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
                Effect,
            },
            execute::execute_effects,
            ExternalEffect, ImmediateSink,
        },
        pending_delivery::{
            flush_for_resource_with_retry, DatabasePendingDeliveryStorage, FlushContext,
            MamArchiveResolver,
        },
    };
    use waddle_xmpp::{
        ingress::IngressEffectIntent,
        pending_delivery::{
            storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
            SmSessionId,
        },
        registry::{RegisterUserResource, UserRegistryActor},
        Stanza,
    };
    let [target, _, _] = delivery::resources();
    let targets = std::slice::from_ref(&target);
    let store: Arc<dyn PendingDeliveryStorage> = Arc::new(
        DatabasePendingDeliveryStorage::open(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
        )
        .await
        .unwrap(),
    );
    for id in ["dispatch-a", "dispatch-b"] {
        let mut offline = submission(&fixture, &fixture.principal, id, targets, false);
        offline
            .plan
            .intents
            .retain(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { .. }));
        offline
            .plan
            .plan
            .retain(|effect| matches!(effect.effect, Effect::Durable(_)));
        let replay = String::from(&minidom::Element::from(
            offline.plan.sanitized_message.clone(),
        ));
        for planned in &mut offline.plan.plan {
            if let Effect::Durable(waddle_server::ingress::DurableEffect::Direct(
                waddle_server::ingress::effects::direct::DurableDirectEffect::ArchiveDirect {
                    archive,
                    message,
                    ..
                },
            )) = &mut planned.effect
            {
                if archive == &target.to_bare() {
                    message.stanza_xml = Some(replay.clone());
                }
            }
        }
        commit_submission(&fixture.uow, &offline, 5).await.unwrap();
        store
            .insert(PendingRow {
                id: PendingRowId::fresh(),
                recipient: target.to_bare(),
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Archived(StanzaId::new(id, target.to_bare().into())),
                flushed_in_session: None,
                outbound_sequence: None,
            })
            .await
            .unwrap();
    }
    let registry = Arc::new(ConnectionRegistry::new());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    registry.register(target.clone(), sender);
    let entry = registry.get_entry(&target).unwrap();
    let stream = SmSessionId::new("ordered-pending-stream");
    entry.set_sm_stream_id(Some(stream.clone()));
    entry
        .presence_available
        .store(true, std::sync::atomic::Ordering::Release);
    let users = UserRegistryActor::spawn(UserRegistryActor::new());
    users
        .ask(RegisterUserResource {
            jid: target.clone(),
            entry: entry.clone(),
        })
        .await
        .unwrap();
    let authority = Arc::new(fixture.authority().await);
    let mam: Arc<dyn waddle_xmpp::mam::MamStorage> = Arc::new(
        waddle_xmpp::mam::SqlxMamStorage::open(fixture.db.database_url())
            .await
            .unwrap(),
    );
    let task = tokio::spawn({
        let store = store.clone();
        let registry = registry.clone();
        let target = target.clone();
        let authority = authority.clone();
        async move {
            let resolver = MamArchiveResolver { mam_storage: mam };
            flush_for_resource_with_retry(
                &store,
                &registry,
                &target.to_bare(),
                &target,
                FlushContext {
                    server_domain: "example.com",
                    sm_session: Some(&stream),
                    blocking_storage: None,
                    owner: Some(&entry.carbons_enabled),
                    archive_resolver: &resolver,
                    dispatch_gate: Some(authority.as_ref()),
                },
            )
            .await
        }
    });
    let mut observed = Vec::new();
    for sequence in [1, 2] {
        let outbound = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let Stanza::Message(message) = outbound.stanza else {
            panic!("message")
        };
        observed.push(
            extract_stanza_ids(&message)
                .into_iter()
                .find(|id| id.by == target.to_bare())
                .unwrap()
                .id,
        );
        // This is the real socket's SM-counted write boundary, deliberately
        // withholding the client's delete_acked_in_window acknowledgement.
        store
            .record_pushed_at(outbound.pending_row_id.as_ref().unwrap(), sequence)
            .await
            .unwrap();
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .pushed,
        2
    );
    assert_eq!(
        store.count(&target.to_bare()).await.unwrap(),
        2,
        "both rows remain unacknowledged"
    );
    let mut live = submission(&fixture, &fixture.principal, "dispatch-c", targets, true);
    live.plan.plan.last_mut().unwrap().effect = Effect::External(ExternalEffect::Delivery(
        ExternalDeliveryEffect::RouteToPeer {
            route_identity: Some(waddle_xmpp::ingress::EffectMessageIdentity::capture_ordinal(0)),
            jid: target.clone(),
            stanza: Box::new(Stanza::Message(live.plan.sanitized_message.clone())),
            kind: PeerDeliveryKind::DirectFrame,
            call_setup: None,
        },
    ));
    let live = commit_submission(&fixture.uow, &live, 5).await.unwrap();
    let mut deps = Deps::new(&registry, "example.com");
    deps.user_registry = Some(&users);
    execute_effects(
        &fixture.uow,
        &fixture.db,
        &live,
        &ImmediateSink,
        &deps,
        Duration::from_secs(3),
    )
    .await;
    let outbound = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let Stanza::Message(message) = outbound.stanza else {
        panic!("live message")
    };
    observed.push(
        extract_stanza_ids(&message)
            .into_iter()
            .find(|id| id.by == target.to_bare())
            .unwrap()
            .id,
    );
    assert_eq!(observed, ["dispatch-a", "dispatch-b", "dispatch-c"]);
    assert_eq!(
        super::query_wire(&fixture, &target.to_bare())
            .await
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        observed
    );
    assert!(authority.drain_and_join(Duration::from_secs(3)).await);
    drop(authority);
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn pending_dispatch_without_ack_sqlite() {
    pending_stream_progress(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn pending_dispatch_without_ack_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ord_ack").await {
        pending_stream_progress(fixture).await;
    }
}
