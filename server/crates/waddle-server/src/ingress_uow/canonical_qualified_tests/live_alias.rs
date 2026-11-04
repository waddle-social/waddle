//! Live prepared commit-path alias races under an attested restricted role.

use super::{source_alias, Fixture};
use crate::ingress::commit::{commit_hooks, commit_submission};
use crate::ingress::decision::{AliasOutcomeClass, IngressDecisionClass};
use crate::ingress::{IngressPrincipal, IngressStreamIdentity, IngressSubmission};
use crate::ingress_substrate::{
    OwnedAliasGate, OwnedAliasSavepointPhase, OWNED_ALIAS_AFTER_INSERT, OWNED_ALIAS_AFTER_LOCK,
    OWNED_ALIAS_AFTER_MISS, OWNED_ALIAS_AFTER_PROBE, OWNED_ALIAS_STALL_SAVEPOINT,
};
use crate::server::routes::interpret::effects::{IngressPlan, RoomExecutionPath};
use futures::FutureExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ingress::{
    ConnectionGeneration, DigestContext, DigestInput, NormalizedTarget, TransportGeneration,
};

fn submission(
    principal: &AuthenticatedPrincipalRef,
    origin: &str,
    body: &str,
) -> IngressSubmission {
    let target = NormalizedTarget::Bare("bob@example.test".parse().expect("target"));
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.test".parse().expect("to")));
    message.from = Some("alice@example.test/phone".parse().expect("from"));
    message.bodies.insert(Default::default(), body.to_owned());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin);
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    IngressSubmission {
        sender: "alice@example.test/phone".parse().expect("sender"),
        identity: IngressStreamIdentity::Ephemeral {
            principal: principal.clone(),
        },
        principal: IngressPrincipal::Authenticated(principal.clone()),
        target,
        plan: IngressPlan {
            failure: None,
            plan: Vec::new(),
            intents: Vec::new(),
            room_canonical_message: None,
            sanitized_message: message,
            rejection: None,
            error_reply: None,
            room_execution: RoomExecutionPath::None,
        },
        digest_input,
        connection_generation: TransportGeneration::Connection(ConnectionGeneration::INITIAL),
    }
}

fn gate() -> OwnedAliasGate {
    OwnedAliasGate {
        entered: Arc::new(Notify::new()),
        proceed: Arc::new(Notify::new()),
    }
}

async fn wait_insert_lock(fixture: &Fixture) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND usename = $2 AND state = 'active' AND wait_event_type = 'Lock' AND query LIKE '%INSERT INTO%ingress_origin_aliases%')")
                .bind(&fixture.database_name).bind(&fixture.runtime).fetch_one(&fixture.admin).await.expect("lock observation");
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }).await.expect("loser waits at alias uniqueness check");
}

async fn message_count(fixture: &Fixture) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.ingress_messages",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("message count")
}

#[tokio::test]
async fn restricted_live_origin_alias_commit_abort_conflict_and_timeout() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
                return;
            };
            let fixture = Fixture::open(&url).await;
            let outcome = std::panic::AssertUnwindSafe(async {
                let (uow, first, second) = source_alias::seed(&fixture, 2).await;
                for (name, abort_winner, conflict, timeout) in [
                    ("live-win", false, false, false),
                    ("live-abort", true, false, false),
                    ("live-conflict", false, true, false),
                    ("live-timeout", false, false, true),
                ] {
                    let before = message_count(&fixture).await;
                    let first_submission = submission(&first, name, "same body");
                    let second_submission = submission(
                        &second,
                        name,
                        if conflict { "other body" } else { "same body" },
                    );
                    let first_miss = gate();
                    let second_miss = gate();
                    let inserted = gate();
                    let winner_uow = uow.clone();
                    let first_gate = first_miss.clone();
                    let insert_gate = inserted.clone();
                    let winner = tokio::task::spawn_local(async move {
                        commit_hooks::ALIAS_TEST_BOUNDS
                            .scope(
                                (Duration::from_secs(8), Duration::from_secs(10)),
                                OWNED_ALIAS_AFTER_MISS.scope(
                                    first_gate,
                                    OWNED_ALIAS_AFTER_INSERT.scope(
                                        insert_gate,
                                        commit_submission(&winner_uow, &first_submission, 1),
                                    ),
                                ),
                            )
                            .await
                    });
                    let loser_uow = uow.clone();
                    let second_gate = second_miss.clone();
                    let observed = Arc::new(std::sync::Mutex::new(None));
                    let loser_observed = observed.clone();
                    let loser = tokio::task::spawn_local(async move {
                        let future = commit_hooks::ALIAS_OBSERVED_BOUNDS.scope(
                            loser_observed,
                            OWNED_ALIAS_AFTER_MISS.scope(
                                second_gate,
                                commit_submission(&loser_uow, &second_submission, 1),
                            ),
                        );
                        if timeout {
                            future.await
                        } else {
                            commit_hooks::ALIAS_TEST_BOUNDS
                                .scope((Duration::from_secs(8), Duration::from_secs(10)), future)
                                .await
                        }
                    });
                    first_miss.entered.notified().await;
                    second_miss.entered.notified().await;
                    assert_eq!(
                        *observed.lock().expect("observed bounds"),
                        Some(if timeout { (100, 250) } else { (8000, 10000) })
                    );
                    first_miss.proceed.notify_one();
                    inserted.entered.notified().await;
                    second_miss.proceed.notify_one();
                    wait_insert_lock(&fixture).await;
                    if timeout {
                        let failure = loser
                            .await
                            .expect("loser join")
                            .expect_err("default lock timeout");
                        assert_eq!(failure.class(), IngressDecisionClass::Timeout);
                        inserted.proceed.notify_one();
                        let winner = winner.await.expect("winner join").expect("winner commit");
                        assert_eq!(winner.alias, AliasOutcomeClass::Inserted);
                        assert_eq!(message_count(&fixture).await, before + 1);
                        continue;
                    }
                    if abort_winner {
                        winner.abort();
                        assert!(winner.await.expect_err("aborted winner").is_cancelled());
                        let result = loser
                            .await
                            .expect("loser join")
                            .expect("loser wins after abort");
                        assert_eq!(result.alias, AliasOutcomeClass::Inserted);
                        assert_eq!(message_count(&fixture).await, before + 1);
                    } else {
                        inserted.proceed.notify_one();
                        let first_result =
                            winner.await.expect("winner join").expect("winner commit");
                        let second_result =
                            loser.await.expect("loser join").expect("loser resolution");
                        assert_eq!(first_result.alias, AliasOutcomeClass::Inserted);
                        if conflict {
                            assert_eq!(second_result.class, IngressDecisionClass::AliasConflict);
                            assert_eq!(second_result.alias, AliasOutcomeClass::Conflict);
                            assert_ne!(first_result.message_key, second_result.message_key);
                            assert_eq!(message_count(&fixture).await, before + 2);
                        } else {
                            assert_eq!(second_result.alias, AliasOutcomeClass::Existing);
                            assert_eq!(first_result.message_key, second_result.message_key);
                            assert_eq!(message_count(&fixture).await, before + 1);
                        }
                    }
                    let (count,): (i64,) = sqlx::query_as(&format!(
                        "SELECT COUNT(*) FROM {}.ingress_origin_aliases WHERE origin_id = $1",
                        fixture.target
                    ))
                    .bind(name)
                    .fetch_one(&fixture.admin)
                    .await
                    .expect("alias count");
                    assert_eq!(count, 1);
                }
            })
            .catch_unwind()
            .await;
            fixture.cleanup().await;
            if let Err(panic) = outcome {
                std::panic::resume_unwind(panic);
            }
        })
        .await;
}

async fn wait_sleep(fixture: &Fixture) -> i64 {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar("SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND usename = $2 AND state = 'active' AND wait_event = 'PgSleep' AND query LIKE '%pg_sleep(3)%' LIMIT 1")
                .bind(&fixture.database_name).bind(&fixture.runtime).fetch_optional(&fixture.admin).await.expect("sleep observation");
            if let Some(pid) = pid { break i64::from(pid); }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("savepoint finished and next statement is in flight")
}

#[tokio::test]
async fn restricted_live_savepoint_cancellation_rolls_back_and_recovers_lane() {
    tokio::task::LocalSet::new().run_until(async {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else { return; };
        let fixture = Fixture::open(&url).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let (uow, first, second) = source_alias::seed(&fixture, 2).await;
            for (phase, name) in [
                (OwnedAliasSavepointPhase::Begin, "cancel-live-begin"),
                (OwnedAliasSavepointPhase::Release, "cancel-live-release"),
                (OwnedAliasSavepointPhase::Rollback, "cancel-live-rollback"),
            ] {
                let before = message_count(&fixture).await;
                let mut reserved = None;
                let mut winner = None;
                let winner_gate = gate();
                if phase == OwnedAliasSavepointPhase::Rollback {
                    let winner_uow = uow.clone();
                    let winner_input = submission(&first, name, "same");
                    let gate_copy = winner_gate.clone();
                    winner = Some(tokio::task::spawn_local(async move {
                        commit_hooks::ALIAS_TEST_BOUNDS.scope((Duration::from_secs(8), Duration::from_secs(10)),
                            OWNED_ALIAS_AFTER_INSERT.scope(gate_copy, commit_submission(&winner_uow, &winner_input, 1))).await
                    }));
                    winner_gate.entered.notified().await;
                } else {
                    reserved = Some(uow.begin().await.expect("reserve other ingress lane"));
                }
                let task_uow = uow.clone();
                let task_input = submission(&second, name, "same");
                let task = tokio::task::spawn_local(async move {
                    commit_hooks::ALIAS_TEST_BOUNDS.scope((Duration::from_secs(8), Duration::from_secs(10)),
                        OWNED_ALIAS_STALL_SAVEPOINT.scope(phase, commit_submission(&task_uow, &task_input, 1))).await
                });
                if let Some(winner) = winner {
                    wait_insert_lock(&fixture).await;
                    winner_gate.proceed.notify_one();
                    winner.await.expect("winner join").expect("winner commits");
                    reserved = Some(uow.begin().await.expect("reserve winner's freed lane"));
                }
                let pid = wait_sleep(&fixture).await;
                task.abort();
                assert!(task.await.expect_err("cancelled commit").is_cancelled());
                assert!(tokio::time::timeout(Duration::from_millis(100), uow.begin()).await.is_err(), "cleanup retains the only free lane");
                let mut reused = tokio::time::timeout(Duration::from_secs(8), uow.begin()).await.expect("lane returns").expect("new UOW");
                let new_pid = super::count(&mut reused, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
                if new_pid != pid {
                    let still_open: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1 AND xact_start IS NOT NULL)")
                        .bind(i32::try_from(pid).expect("pid")).fetch_one(&fixture.admin).await.expect("old backend state");
                    assert!(!still_open, "old backend must no longer hold its transaction");
                }
                reused.commit().await.expect("reused UOW is usable");
                let expected = before + i64::from(phase == OwnedAliasSavepointPhase::Rollback);
                assert_eq!(message_count(&fixture).await, expected, "no cancelled candidate persists");
                let aliases: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}.ingress_origin_aliases WHERE origin_id = $1", fixture.target))
                    .bind(name).fetch_one(&fixture.admin).await.expect("alias count");
                assert_eq!(aliases, i64::from(phase == OwnedAliasSavepointPhase::Rollback));
                drop(reserved);
            }
        }).catch_unwind().await;
        fixture.cleanup().await;
        if let Err(panic) = outcome { std::panic::resume_unwind(panic); }
    }).await;
}

#[tokio::test]
async fn restricted_live_stale_probe_fails_closed_after_gc() {
    tokio::task::LocalSet::new().run_until(async {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else { return; };
        let fixture = Fixture::open(&url).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let (uow, principal, _) = source_alias::seed(&fixture, 2).await;
            let input = submission(&principal, "live-gc", "same");
            let initial = commit_submission(&uow, &input, 1).await.expect("seed message");
            let key = initial.message_key.expect("message key");
            let before = message_count(&fixture).await;
            let probe = gate();
            let probe_copy = probe.clone();
            let task_uow = uow.clone();
            let task = tokio::task::spawn_local(async move {
                commit_hooks::ALIAS_TEST_BOUNDS.scope((Duration::from_secs(8), Duration::from_secs(10)),
                    OWNED_ALIAS_AFTER_PROBE.scope(probe_copy, commit_submission(&task_uow, &input, 1))).await
            });
            probe.entered.notified().await;
            let mut gc = fixture.admin.begin().await.expect("GC tx");
            sqlx::query(&format!("SELECT 1 FROM {}.ingress_messages WHERE message_key = $1::uuid FOR UPDATE", fixture.target))
                .bind(key.to_storage().to_string()).fetch_one(&mut *gc).await.expect("parent lock");
            sqlx::query(&format!("DELETE FROM {}.ingress_origin_aliases WHERE message_key = $1::uuid", fixture.target))
                .bind(key.to_storage().to_string()).execute(&mut *gc).await.expect("delete alias");
            sqlx::query(&format!("DELETE FROM {}.ingress_messages WHERE message_key = $1::uuid", fixture.target))
                .bind(key.to_storage().to_string()).execute(&mut *gc).await.expect("delete parent");
            probe.proceed.notify_one();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=$1 AND usename=$2 AND state='active' AND wait_event_type='Lock' AND query LIKE '%ingress_messages%FOR UPDATE%')")
                        .bind(&fixture.database_name).bind(&fixture.runtime).fetch_one(&fixture.admin).await.expect("wait state");
                    if waiting { break; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.expect("parent lock wait");
            gc.commit().await.expect("GC commit");
            let failure = task.await.expect("task join").expect_err("stale probe fails closed");
            assert!(matches!(failure.source, crate::ingress_uow::IngressUowError::Substrate(crate::ingress_substrate::IngressSubstrateError::AliasMissingAfterConflict)));
            assert_eq!(message_count(&fixture).await, before - 1, "no replacement candidate minted");
        }).catch_unwind().await;
        fixture.cleanup().await;
        if let Err(panic) = outcome { std::panic::resume_unwind(panic); }
    }).await;
}

#[tokio::test]
async fn restricted_live_replay_denial_lost_ack_and_qualified_relations() {
    tokio::task::LocalSet::new().run_until(async {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else { return; };
        let fixture = Fixture::open(&url).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let (uow, principal, _) = source_alias::seed(&fixture, 1).await;
            let normal = submission(&principal, "live-replay", "body");
            let first = commit_submission(&uow, &normal, 1).await.expect("first commit");
            let key = first.message_key.expect("message key");
            assert_eq!(first.alias, AliasOutcomeClass::Inserted);
            let (envelope, source_version): (Option<Vec<u8>>, Option<i32>) = sqlx::query_as(&format!("SELECT envelope, source_binding_version::int FROM {}.ingress_messages WHERE message_key=$1::uuid", fixture.target))
                .bind(key.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("stored envelope");
            assert!(envelope.is_some());
            assert!(source_version.is_none());
            let replay = commit_submission(&uow, &normal, 1).await.expect("replay");
            assert_eq!(replay.alias, AliasOutcomeClass::Existing);
            assert_eq!(replay.message_key, Some(key));
            let after: Option<Vec<u8>> = sqlx::query_scalar(&format!("SELECT envelope FROM {}.ingress_messages WHERE message_key=$1::uuid", fixture.target))
                .bind(key.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("replay envelope");
            assert_eq!(after, envelope, "replay must not overwrite the existing envelope");

            use crate::server::routes::interpret::effects::{PlanRejection, PolicyDeniedReason};
            use waddle_xmpp::ingress::{FrozenStanzaError, FrozenStanzaErrorType, IngressEffectIntent};
            let denied_offer = |origin| {
                let mut denied = submission(&principal, origin, "body");
                let error = FrozenStanzaError::new(FrozenStanzaErrorType::Cancel, waddle_xmpp::StanzaErrorCondition::Forbidden);
                let mut reply = denied.plan.sanitized_message.clone();
                reply.type_ = xmpp_parsers::message::MessageType::Error;
                reply.payloads.push(error.to_xmpp().into());
                denied.plan.rejection = Some(PlanRejection::PolicyDenied(PolicyDeniedReason::OperationalFenceLoss));
                denied.plan.error_reply = Some(waddle_xmpp::Stanza::Message(reply));
                denied.plan.intents.push(IngressEffectIntent::ErrorReply { recipient: "alice@example.test/phone".parse().expect("recipient"), error });
                denied
            };
            let denied = denied_offer("live-denial");
            let denial = commit_submission(&uow, &denied, 1).await.expect("denial commit");
            assert_eq!(denial.class, IngressDecisionClass::PolicyDenied);
            assert_eq!(denial.alias, AliasOutcomeClass::Inserted);
            let denial_replay = commit_submission(&uow, &denied, 1).await.expect("denial replay");
            assert_eq!(denial_replay.alias, AliasOutcomeClass::Existing);
            assert_eq!(denial.message_key, denial_replay.message_key);
            let changed_policy = commit_submission(&uow, &denied_offer("live-replay"), 1).await.expect("acceptance replay under denial");
            assert_eq!(changed_policy.alias, AliasOutcomeClass::Existing);
            assert_eq!(changed_policy.message_key, Some(key));
            assert_eq!(changed_policy.class, IngressDecisionClass::ExistingConsistent);

            let ambiguous = submission(&principal, "live-ambiguous", "body");
            let before = message_count(&fixture).await;
            let failure = commit_hooks::AMBIGUOUS_COMMIT.scope(true, commit_submission(&uow, &ambiguous, 1)).await.expect_err("lost acknowledged commit reply");
            assert_eq!(failure.class(), IngressDecisionClass::AmbiguousCommit);
            assert_eq!(message_count(&fixture).await, before + 1);

            // TEMP shadows are created after attestation on the same transaction.
            let mut tx = uow.begin().await.expect("prepared UOW");
            fixture.seed_shadows(&mut tx).await;
            let temp_offer = submission(&principal, "live-temp-shadow", "body");
            let digest = waddle_xmpp::ingress::digest::v1::digest(&temp_offer.digest_input);
            let (mut tx, resolution) = super::CanonicalMessageRepository::resolve_prepared_origin_owned(
                tx, principal.bare_jid(), &temp_offer.target, temp_offer.digest_input.origin().expect("origin"), &digest,
            ).await.expect("qualified candidate");
            let legacy_key = match resolution {
                waddle_xmpp::ingress::AliasResolution::Aliased(waddle_xmpp::ingress::AliasOutcome::Inserted(key)) => key,
                other => panic!("expected inserted legacy candidate, got {other:?}"),
            };
            assert_eq!(super::count(&mut tx, "SELECT COUNT(*) FROM pg_temp.ingress_messages").await, 0);
            assert_eq!(super::count(&mut tx, "SELECT COUNT(*) FROM pg_temp.ingress_origin_aliases").await, 0);
            tx.commit().await.expect("commit qualified candidate");
            let legacy_replay = commit_submission(&uow, &temp_offer, 1).await.expect_err("legacy parent without envelope remains invalid");
            assert!(matches!(legacy_replay.source, crate::ingress_uow::IngressUowError::EffectIntentMessageMissing));
            let legacy_envelope: Option<Vec<u8>> = sqlx::query_scalar(&format!("SELECT envelope FROM {}.ingress_messages WHERE message_key=$1::uuid", fixture.target))
                .bind(legacy_key.to_storage().to_string()).fetch_one(&fixture.admin).await.expect("legacy envelope");
            assert!(legacy_envelope.is_none(), "replay must not fill a legacy parent envelope");

            sqlx::query(&format!("ALTER TABLE {}.ingress_origin_aliases RENAME TO hidden_aliases", fixture.target)).execute(&fixture.admin).await.expect("hide target alias");
            let before_missing = message_count(&fixture).await;
            let missing = submission(&principal, "live-missing", "body");
            let failure = commit_submission(&uow, &missing, 1).await.expect_err("missing target alias relation");
            assert_eq!(failure.class(), IngressDecisionClass::Storage);
            assert_eq!(message_count(&fixture).await, before_missing);
            sqlx::query(&format!("ALTER TABLE {}.hidden_aliases RENAME TO ingress_origin_aliases", fixture.target)).execute(&fixture.admin).await.expect("restore target alias");
        }).catch_unwind().await;
        fixture.cleanup().await;
        if let Err(panic) = outcome { std::panic::resume_unwind(panic); }
    }).await;
}

#[tokio::test]
async fn prepared_existing_alias_holds_parent_until_sm_ref_is_committed() {
    tokio::task::LocalSet::new().run_until(async {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else { return; };
        let fixture = Fixture::open(&url).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let (uow, principal, _) = source_alias::seed(&fixture, 2).await;
            // SM stream mutation has a broader role contract than the
            // delete-free ephemeral alias fixture.
            sqlx::query(&format!("GRANT UPDATE ON {}.ingress_sm_streams TO {}", fixture.target, fixture.runtime))
                .execute(&fixture.admin).await.expect("stream update grant");
            let initial = submission(&principal, "live-sm-gc", "same");
            let first = commit_submission(&uow, &initial, 1).await.expect("seed canonical");
            let key = first.message_key.expect("canonical key");
            let mut resumed = initial.clone();
            resumed.identity = IngressStreamIdentity::Resumable {
                stream_id: waddle_xmpp::pending_delivery::SmSessionId::new("live-sm-gc-stream"),
                sm_ingress_id: waddle_xmpp::ingress::SmIngressId::new(),
                #[cfg(feature = "clustering")]
                owner: waddle_xmpp::ownership::NodeIdentity::new("single-node", "single-incarnation"),
                #[cfg(feature = "clustering")]
                claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
                reserved_wire_position: waddle_xmpp::ingress::WireHandledCount::from_storage(1),
                checkpoint_h: waddle_xmpp::ingress::WireHandledCount::from_storage(1),
            };
            let locked = gate();
            let gate_copy = locked.clone();
            let task_uow = uow.clone();
            let offer = tokio::task::spawn_local(async move {
                commit_hooks::ALIAS_TEST_BOUNDS.scope((Duration::from_secs(8), Duration::from_secs(10)),
                    OWNED_ALIAS_AFTER_LOCK.scope(gate_copy, commit_submission(&task_uow, &resumed, 1))).await
            });
            locked.entered.notified().await;
            let admin = fixture.admin.clone();
            let schema = fixture.target.clone();
            let (pid_sender, pid_receiver) = tokio::sync::oneshot::channel();
            let gc = tokio::task::spawn_local(async move {
                let mut tx = admin.begin().await.expect("GC tx");
                let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()").fetch_one(&mut *tx).await.expect("GC pid");
                pid_sender.send(i64::from(pid)).expect("send GC pid");
                sqlx::query(&format!("SELECT 1 FROM {schema}.ingress_messages WHERE message_key=$1::uuid FOR UPDATE"))
                    .bind(key.to_storage().to_string()).fetch_one(&mut *tx).await.expect("GC parent lock");
                let refs: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {schema}.ingress_sm_refs WHERE message_key=$1::uuid"))
                    .bind(key.to_storage().to_string()).fetch_one(&mut *tx).await.expect("observe SM references");
                tx.rollback().await.expect("finish GC observation");
                refs
            });
            let pid = pid_receiver.await.expect("GC pid");
            source_alias::wait_backend_query(&fixture, pid, "FOR UPDATE", "Lock").await;
            locked.proceed.notify_one();
            let replay = offer.await.expect("offer join").expect("commit SM reference");
            assert_eq!(replay.alias, AliasOutcomeClass::Existing);
            assert_eq!(replay.message_key, Some(key));
            assert_eq!(gc.await.expect("GC join"), 1, "GC's parent lock must precede observation of the committed child");
        }).catch_unwind().await;
        fixture.cleanup().await;
        if let Err(panic) = outcome { std::panic::resume_unwind(panic); }
    }).await;
}

#[test]
fn owned_alias_future_is_send() {
    fn require_send<T: Send>(_: T) {}
    fn prove<'a>(
        tx: super::IngressUowTransaction<'a>,
        sender: &jid::BareJid,
        target: &NormalizedTarget,
        origin: &waddle_xmpp_core::xep0359::OriginId,
        digest: &waddle_xmpp::ingress::SemanticDigest,
    ) {
        require_send(
            super::CanonicalMessageRepository::resolve_prepared_origin_owned(
                tx, sender, target, origin, digest,
            ),
        );
    }
    let _ = prove;
}
