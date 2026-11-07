//! Real portable-store coverage for the unselected SM revision boundary.
use std::time::Duration;

use uuid::Uuid;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::sm_persistence::revisioned::{
    bootstrap_revisioned_sm_owner_schema, Effect, Mutation, Readback, Request, RevisionError,
    RevisionedSmStorage,
};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, PersistedDetachedAppend, PersistedIngressAppend, PersistedSession,
    PersistedUnackedStanza, SmAckWindow, SmAcknowledgementMode,
};
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};
use waddle_xmpp::tombstone::TombstoneTarget;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;

fn principal() -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        "alice@example.com".parse().expect("bare JID"),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::INITIAL,
    )
}

fn session(stream: &str) -> PersistedSession {
    PersistedSession {
        stream_id: SmSessionId::new(stream),
        user_id: "alice".to_owned(),
        jid: "alice@example.com/web".parse().expect("full JID"),
        occupancy_session: OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 1,
        last_acked: 0,
        replay_gap_through: None,
        max_resume_time: Some(60),
        detached_at: chrono::Utc::now(),
        max_resume_duration: Duration::from_secs(60),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
    }
}

fn frame(stream: &SmSessionId, sequence: u32) -> PersistedUnackedStanza {
    let mut message = xmpp_parsers::message::Message::new(Some(
        "alice@example.com/web".parse().expect("recipient"),
    ));
    message.from = Some("bob@example.com/phone".parse().expect("sender"));
    message.id = Some(xmpp_parsers::message::Id(format!("frame-{sequence}")));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        format!("frame-{sequence}"),
    );
    PersistedUnackedStanza {
        stream_id: stream.clone(),
        sequence,
        stanza: Box::new(Stanza::Message(message)),
        original_receipt_at: chrono::Utc::now(),
        ingress_receipts: Vec::new(),
        provenance: Default::default(),
    }
}

fn proof(session: &PersistedSession, frame: &PersistedUnackedStanza) -> PersistedIngressAppend {
    PersistedIngressAppend {
        key: SmIngressAppendKey {
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(1),
            semantic_identity_hash: [5; 32],
            resource: session.jid.clone(),
        },
        accepting_stream: session.stream_id.clone(),
        sequence: frame.sequence,
        appended_at: chrono::Utc::now(),
        payload: (*frame.stanza).clone(),
        original_receipt_at: frame.original_receipt_at,
        disposition: IngressCustodyDisposition::Pending,
    }
}

async fn initialize(db: &Database) {
    waddle_server::sm_persistence::bootstrap_portable_sm_owner_schema(db)
        .await
        .expect("portable schema");
    bootstrap_revisioned_sm_owner_schema(db)
        .await
        .expect("revision schema");
}

async fn exercise(db: Database, second_db: Database) {
    initialize(&db).await;
    assert!(
        bootstrap_revisioned_sm_owner_schema(&db).await.is_err(),
        "owner bootstrap must refuse pre-existing authority tables"
    );
    let blocker_db = db.clone();
    let a = RevisionedSmStorage::new(db);
    let b = RevisionedSmStorage::new(second_db);
    let principal = principal();
    let initial = session("revision-stream");
    let initial_frames = vec![frame(&initial.stream_id, 1)];
    let create = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &initial,
            unacked: &initial_frames,
            drained: &[],
        },
    };
    let created = a.apply(&create).await.expect("create");
    assert_eq!(created.revision, 1);
    assert_eq!(
        a.readback(&create).await.expect("create receipt"),
        Readback::Applied(created)
    );
    let capture = b
        .capture(&initial.stream_id)
        .await
        .expect("consistent capture");
    assert_eq!(capture.revision.value(), 1);
    assert_eq!(capture.unacked.len(), 1);
    assert_eq!(capture.unacked[0].sequence, 1);

    // A different connection's successful append invalidates the older full
    // snapshot, preserving the new frame and counters.
    let mut appended = capture.session.clone();
    appended.outbound_count = 2;
    let append = PersistedDetachedAppend {
        session: appended,
        stanza: frame(&initial.stream_id, 2),
        evicted_sequences: Vec::new(),
        ingress_append: None,
        new_delivery: false,
    };
    let append_request = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Append {
            revision: &capture.revision,
            append: &append,
        },
    };
    let append_result = a.apply(&append_request).await.expect("append");
    assert_eq!(append_result.effect, Effect::Appended);
    let stale = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Snapshot {
            revision: &capture.revision,
            session: &capture.session,
            unacked: &capture.unacked,
            drained: &[],
        },
    };
    assert!(matches!(
        b.apply(&stale).await,
        Err(RevisionError::StaleRevision)
    ));
    let current = b
        .capture(&initial.stream_id)
        .await
        .expect("post-append capture");
    assert_eq!(current.session.outbound_count, 2);
    assert_eq!(
        current
            .unacked
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let omitted = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Snapshot {
            revision: &current.revision,
            session: &current.session,
            unacked: &current.unacked[..1],
            drained: &[],
        },
    };
    assert!(matches!(
        a.apply(&omitted).await,
        Err(RevisionError::InvalidInput)
    ));
    assert_eq!(
        b.capture(&initial.stream_id)
            .await
            .expect("unchanged after omission")
            .revision
            .value(),
        current.revision.value()
    );

    // Reverse the order: a successful full snapshot fences the older append.
    let mut changed = current.session.clone();
    changed.inbound_count = 1;
    let snapshot = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Snapshot {
            revision: &current.revision,
            session: &changed,
            unacked: &current.unacked,
            drained: &[],
        },
    };
    let mut candidate_session = current.session.clone();
    candidate_session.outbound_count = 3;
    let later_append = PersistedDetachedAppend {
        session: candidate_session,
        stanza: frame(&initial.stream_id, 3),
        evicted_sequences: Vec::new(),
        ingress_append: None,
        new_delivery: false,
    };
    let stale_append = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Append {
            revision: &current.revision,
            append: &later_append,
        },
    };
    let mut needs_delivery_authority = later_append.clone();
    needs_delivery_authority.ingress_append = Some(proof(
        &needs_delivery_authority.session,
        &needs_delivery_authority.stanza,
    ));
    needs_delivery_authority.new_delivery = true;
    let unsupported = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Append {
            revision: &current.revision,
            append: &needs_delivery_authority,
        },
    };
    assert!(matches!(
        b.apply(&unsupported).await,
        Err(RevisionError::Unsupported)
    ));
    b.apply(&snapshot).await.expect("snapshot");
    assert!(matches!(
        a.apply(&stale_append).await,
        Err(RevisionError::StaleRevision)
    ));
    assert_eq!(
        a.apply(&snapshot).await.expect("idempotent snapshot"),
        match a.readback(&snapshot).await.expect("snapshot receipt") {
            Readback::Applied(result) => result,
            Readback::Unconfirmed => panic!("snapshot receipt missing"),
        }
    );
    let different = Request {
        id: snapshot.id,
        mutation: Mutation::Snapshot {
            revision: &current.revision,
            session: &current.session,
            unacked: &current.unacked,
            drained: &[],
        },
    };
    assert!(matches!(
        a.readback(&different).await,
        Err(RevisionError::OperationIdentity)
    ));

    let current = a.capture(&initial.stream_id).await.expect("before ACK");
    let outstanding = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Retire {
            revision: &current.revision,
        },
    };
    assert!(matches!(
        b.apply(&outstanding).await,
        Err(RevisionError::OutstandingCustody)
    ));
    let ack = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Acknowledge {
            revision: &current.revision,
            h: 1,
            mode: SmAcknowledgementMode::PreserveResumability,
        },
    };
    b.apply(&ack).await.expect("acknowledge");
    let current = a.capture(&initial.stream_id).await.expect("after ACK");
    assert_eq!(current.session.last_acked, 1);
    assert_eq!(current.unacked.len(), 1);
    assert_eq!(current.unacked[0].sequence, 2);
    let premature_retire = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Retire {
            revision: &current.revision,
        },
    };
    assert!(matches!(
        b.apply(&premature_retire).await,
        Err(RevisionError::Storage(_))
    ));
    let confirm = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::ConfirmAcknowledgement {
            revision: &current.revision,
            window: SmAckWindow {
                from_exclusive: 0,
                to_inclusive: 1,
            },
        },
    };
    assert!(matches!(
        a.apply(&confirm).await,
        Err(RevisionError::Unsupported)
    ));
    let invalid_tombstone = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Tombstone {
            revision: &current.revision,
            target: &TombstoneTarget::Direct {
                wire_id: "another-frame".to_owned(),
                author: "bob@example.com".parse().expect("author"),
                archive: "alice@example.com".parse().expect("archive"),
            },
            through: chrono::Utc::now(),
            sequences: &[2],
        },
    };
    assert!(matches!(
        a.apply(&invalid_tombstone).await,
        Err(RevisionError::Unsupported)
    ));
    let tombstone = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Tombstone {
            revision: &current.revision,
            target: &TombstoneTarget::Direct {
                wire_id: "frame-2".to_owned(),
                author: "bob@example.com".parse().expect("author"),
                archive: "alice@example.com".parse().expect("archive"),
            },
            through: chrono::Utc::now(),
            sequences: &[2],
        },
    };
    assert!(matches!(
        a.apply(&tombstone).await,
        Err(RevisionError::Unsupported)
    ));
    let current = b
        .capture(&initial.stream_id)
        .await
        .expect("after tombstone");
    assert_eq!(current.unacked.len(), 1);
    let prune = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Prune {
            revision: &current.revision,
            sequences: &[2],
        },
    };
    assert!(matches!(
        a.apply(&prune).await,
        Err(RevisionError::Unsupported)
    ));
    let blocked_retire = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Retire {
            revision: &current.revision,
        },
    };
    assert!(matches!(
        b.apply(&blocked_retire).await,
        Err(RevisionError::Storage(_))
    ));

    let mut terminal_session = session("terminal-stream");
    terminal_session.outbound_count = 0;
    let terminal_frames = Vec::new();
    let terminal_create = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &terminal_session,
            unacked: &terminal_frames,
            drained: &[],
        },
    };
    b.apply(&terminal_create)
        .await
        .expect("create terminal fixture");
    let current = b
        .capture(&terminal_session.stream_id)
        .await
        .expect("capture terminal fixture");
    let retire = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Retire {
            revision: &current.revision,
        },
    };
    let retired = a.apply(&retire).await.expect("retire");
    assert_eq!(retired.effect, Effect::Retired);
    assert_eq!(
        b.readback(&retire).await.expect("retire receipt"),
        Readback::Applied(retired)
    );
    assert!(matches!(
        b.capture(&terminal_session.stream_id).await,
        Err(RevisionError::Terminal)
    ));
    let reused_occupancy = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &terminal_session,
            unacked: &terminal_frames,
            drained: &[],
        },
    };
    assert!(matches!(
        b.apply(&reused_occupancy).await,
        Err(RevisionError::GenerationChanged)
    ));
    let mut replacement = terminal_session.clone();
    replacement.occupancy_session = OccupancySessionGeneration::mint();
    let reopened = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &replacement,
            unacked: &terminal_frames,
            drained: &[],
        },
    };
    b.apply(&reopened).await.expect("new generation");
    assert!(
        matches!(a.apply(&retire).await, Ok(_)),
        "exact old operation remains readable"
    );
    let stale_retire = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Retire {
            revision: &current.revision,
        },
    };
    assert!(matches!(
        a.apply(&stale_retire).await,
        Err(RevisionError::GenerationChanged)
    ));

    // A missing readback while the writer waits is not rollback proof. Hold
    // the actual database gate, start an operation on the other pool, observe
    // Unconfirmed, then let that very operation commit and observe its receipt.
    let delayed_capture = a
        .capture(&terminal_session.stream_id)
        .await
        .expect("delayed capture");
    let mut delayed_session = delayed_capture.session.clone();
    delayed_session.inbound_count += 1;
    let delayed_id = Uuid::new_v4();
    let delayed_request = Request {
        id: delayed_id,
        mutation: Mutation::Snapshot {
            revision: &delayed_capture.revision,
            session: &delayed_session,
            unacked: &delayed_capture.unacked,
            drained: &[],
        },
    };
    let mut blocker = blocker_db
        .begin_immediate()
        .await
        .expect("blocking transaction");
    if blocker.driver() == DatabaseDriver::Postgres {
        blocker
            .execute(
                "SELECT pg_catalog.pg_advisory_xact_lock(1463895108, pg_catalog.hashtext(?))",
                waddle_server::db_params![terminal_session.stream_id.as_str()],
            )
            .await
            .expect("hold stream gate");
    }
    let worker = b.clone();
    let worker_revision = delayed_capture.revision.clone();
    let worker_session = delayed_session.clone();
    let worker_unacked = delayed_capture.unacked.clone();
    let mut pending = tokio::spawn(async move {
        let request = Request {
            id: delayed_id,
            mutation: Mutation::Snapshot {
                revision: &worker_revision,
                session: &worker_session,
                unacked: &worker_unacked,
                drained: &[],
            },
        };
        worker.apply(&request).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut pending)
            .await
            .is_err(),
        "the writer must wait at the database gate"
    );
    assert_eq!(
        a.readback(&delayed_request)
            .await
            .expect("unconfirmed read"),
        Readback::Unconfirmed
    );
    blocker.commit().await.expect("release stream gate");
    let result = pending.await.expect("writer task").expect("delayed commit");
    assert_eq!(
        a.readback(&delayed_request)
            .await
            .expect("positive readback"),
        Readback::Applied(result)
    );

    // A counted drained frame remains in the snapshot when a different
    // stream already owns the custody key; the withheld result is durable.
    let one = session("drained-one");
    let one_frames = vec![frame(&one.stream_id, 1)];
    let first_proof = proof(&one, &one_frames[0]);
    let first_drained = vec![first_proof.clone()];
    let first = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &one,
            unacked: &one_frames,
            drained: &first_drained,
        },
    };
    a.apply(&first).await.expect("first drained allocation");
    let two = session("drained-two");
    let two_frames = vec![frame(&two.stream_id, 1)];
    let mut conflict = first_proof.clone();
    conflict.accepting_stream = two.stream_id.clone();
    conflict.payload = (*two_frames[0].stanza).clone();
    conflict.original_receipt_at = two_frames[0].original_receipt_at;
    let second_drained = vec![conflict];
    let second = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &two,
            unacked: &two_frames,
            drained: &second_drained,
        },
    };
    let outcome = b.apply(&second).await.expect("withheld snapshot");
    assert_eq!(
        outcome.effect,
        Effect::Snapshot {
            withheld: vec![first_proof.key.clone()]
        }
    );
    assert_eq!(
        a.readback(&second).await.expect("withheld receipt"),
        Readback::Applied(outcome.clone())
    );
    assert_eq!(
        a.capture(&two.stream_id)
            .await
            .expect("withheld capture")
            .unacked
            .len(),
        1
    );
    let current = a.capture(&two.stream_id).await.expect("withheld frame");
    let retract = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Tombstone {
            revision: &current.revision,
            target: &TombstoneTarget::Direct {
                wire_id: "frame-1".to_owned(),
                author: "bob@example.com".parse().expect("author"),
                archive: "alice@example.com".parse().expect("archive"),
            },
            through: chrono::Utc::now(),
            sequences: &[1],
        },
    };
    assert!(matches!(
        a.apply(&retract).await,
        Err(RevisionError::Unsupported)
    ));
    assert_eq!(
        a.readback(&second).await.expect("retained withheld result"),
        Readback::Applied(outcome)
    );

    let overflow_session = session("revision-overflow");
    let overflow_frames = vec![frame(&overflow_session.stream_id, 1)];
    let create_overflow = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &overflow_session,
            unacked: &overflow_frames,
            drained: &[],
        },
    };
    a.apply(&create_overflow)
        .await
        .expect("create overflow fixture");
    let mut tx = blocker_db
        .begin_immediate()
        .await
        .expect("set overflow fixture");
    tx.execute(
        "UPDATE sm_revision_authority SET revision = ? WHERE stream_id = ?",
        waddle_server::db_params![i64::MAX, overflow_session.stream_id.as_str()],
    )
    .await
    .expect("set maximum revision");
    tx.commit().await.expect("commit maximum revision");
    let captured = b
        .capture(&overflow_session.stream_id)
        .await
        .expect("capture max revision");
    let rejected = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Snapshot {
            revision: &captured.revision,
            session: &captured.session,
            unacked: &captured.unacked,
            drained: &[],
        },
    };
    assert!(matches!(
        a.apply(&rejected).await,
        Err(RevisionError::RevisionExhausted)
    ));

    let corrupt_session = session("revision-corrupt");
    let corrupt_frames = vec![frame(&corrupt_session.stream_id, 1)];
    let create_corrupt = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &corrupt_session,
            unacked: &corrupt_frames,
            drained: &[],
        },
    };
    a.apply(&create_corrupt)
        .await
        .expect("create corruption fixture");
    let mut tx = blocker_db
        .begin_immediate()
        .await
        .expect("corrupt provenance fixture");
    tx.execute(
        "UPDATE sm_unacked SET provenance = ? WHERE stream_id = ? AND sequence = ?",
        waddle_server::db_params![vec![0_u8, 1, 2], corrupt_session.stream_id.as_str(), 1_i64],
    )
    .await
    .expect("write malformed provenance");
    tx.commit().await.expect("commit malformed provenance");
    assert!(matches!(
        b.capture(&corrupt_session.stream_id).await,
        Err(RevisionError::InvalidInput)
    ));

    let orphan_session = session("revision-orphan");
    let orphan_frames = vec![frame(&orphan_session.stream_id, 1)];
    let orphan = proof(&orphan_session, &orphan_frames[0]);
    let xml = waddle_xmpp::parser::element_to_string(&orphan.payload.to_element())
        .expect("serialize custody payload");
    let mut tx = blocker_db.begin_immediate().await.expect("orphan fixture");
    tx.execute(
        "INSERT INTO sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        waddle_server::db_params![orphan.key.message_key.to_storage().to_string(), i64::from(orphan.key.kind.to_storage()), orphan.key.semantic_identity_hash.to_vec(), orphan.key.resource.to_string(), orphan_session.stream_id.as_str(), 1_i64, orphan.appended_at.timestamp_millis(), xml, orphan.original_receipt_at.timestamp_millis(), 3_i64],
    ).await.expect("insert orphan custody");
    tx.commit().await.expect("commit orphan custody");
    let refused = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &orphan_session,
            unacked: &orphan_frames,
            drained: &[],
        },
    };
    assert!(matches!(
        a.apply(&refused).await,
        Err(RevisionError::GenerationChanged)
    ));

    exercise_watermarks_and_coverage(&a, &b, &blocker_db, &principal).await;
}

#[tokio::test]
async fn sqlite_revision_participation_across_independent_pools() {
    let directory = tempfile::tempdir().expect("SQLite directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("sm.db").display()
    );
    let config = DatabaseConfig::new(DatabaseDriver::Sqlite, url);
    let a = Database::from_config("revision sqlite a", &config)
        .await
        .expect("first pool");
    let b = Database::from_config("revision sqlite b", &config)
        .await
        .expect("second pool");
    exercise(a, b).await;
}

#[tokio::test]
async fn postgres_revision_participation_across_independent_pools() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let admin = sqlx::PgPool::connect(&url).await.expect("PostgreSQL admin");
    let schema = format!("waddle_sm_revision_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("schema");
    let mut scoped = url::Url::parse(&url).expect("PostgreSQL URL");
    let retained: Vec<(String, String)> = scoped
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    scoped
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("options", &format!("-c search_path={schema}"));
    let config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped.to_string());
    let a = Database::from_config("revision postgres a", &config)
        .await
        .expect("first pool");
    let b = Database::from_config("revision postgres b", &config)
        .await
        .expect("second pool");
    exercise(a, b).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
}

#[path = "../src/test_support/postgres_proxy.rs"]
mod postgres_proxy;

#[tokio::test]
async fn postgres_revision_readback_after_backend_commit_reply_is_lost() {
    use std::str::FromStr;

    use sqlx::postgres::PgConnectOptions;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let admin = sqlx::PgPool::connect(&url).await.expect("PostgreSQL admin");
    let schema = format!("waddle_sm_revision_reply_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("schema");
    let mut scoped = url::Url::parse(&url).expect("PostgreSQL URL");
    let retained: Vec<(String, String)> = scoped
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    scoped
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("options", &format!("-c search_path={schema}"));
    let config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped.to_string());
    let db = Database::from_config("revision direct", &config)
        .await
        .expect("direct pool");
    initialize(&db).await;
    let direct = RevisionedSmStorage::new(db);
    let principal = principal();
    let session = session("lost-commit-reply");
    let frames = vec![frame(&session.stream_id, 1)];
    let drained = vec![proof(&session, &frames[0])];
    let create = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal: &principal,
            session: &session,
            unacked: &frames,
            drained: &drained,
        },
    };
    direct.apply(&create).await.expect("create initial state");
    let capture = direct.capture(&session.stream_id).await.expect("capture");
    let mut updated = capture.session.clone();
    updated.inbound_count += 1;
    let request = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Snapshot {
            revision: &capture.revision,
            session: &updated,
            unacked: &capture.unacked,
            drained: &drained,
        },
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("loopback proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let upstream = PgConnectOptions::from_str(&url).expect("upstream options");
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.expect("proxy client");
        let server = postgres_proxy::connect_local_postgres(&upstream)
            .await
            .expect("local PostgreSQL upstream");
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let forward = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
        });
        loop {
            let tag = server_read.read_u8().await.expect("server tag");
            let length = server_read.read_u32().await.expect("server length");
            assert!((4..=1_048_576).contains(&length));
            let mut body = vec![0; usize::try_from(length - 4).expect("message length")];
            server_read
                .read_exact(&mut body)
                .await
                .expect("server body");
            if tag == b'C' && body == b"COMMIT\0" {
                break;
            }
            client_write.write_u8(tag).await.expect("forward tag");
            client_write
                .write_u32(length)
                .await
                .expect("forward length");
            client_write.write_all(&body).await.expect("forward body");
        }
        forward.abort();
        let _ = forward.await;
        client_write.shutdown().await.expect("drop commit reply");
    });
    let proxy_url = postgres_proxy::local_proxy_url(scoped.as_str(), port)
        .expect("local proxy URL")
        .to_string();
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, proxy_url);
    config.pool_size = 1;
    let proxied = RevisionedSmStorage::new(
        Database::from_config("revision commit proxy", &config)
            .await
            .expect("proxied pool"),
    );
    assert!(
        proxied.apply(&request).await.is_err(),
        "commit reply is lost"
    );
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy completes")
        .expect("proxy observed backend COMMIT");
    let expected = match direct.readback(&request).await.expect("exact readback") {
        Readback::Applied(result) => result,
        Readback::Unconfirmed => panic!("backend commit must have a receipt"),
    };
    assert_eq!(
        expected.effect,
        Effect::Snapshot {
            withheld: vec![drained[0].key.clone()]
        }
    );
    assert_eq!(direct.apply(&request).await.expect("exact retry"), expected);
    assert_eq!(
        direct
            .capture(&session.stream_id)
            .await
            .expect("committed state")
            .session
            .inbound_count,
        updated.inbound_count
    );
    let mut altered = updated.clone();
    altered.inbound_count += 1;
    let different = Request {
        id: request.id,
        mutation: Mutation::Snapshot {
            revision: &capture.revision,
            session: &altered,
            unacked: &capture.unacked,
            drained: &drained,
        },
    };
    assert!(matches!(
        direct.readback(&different).await,
        Err(RevisionError::OperationIdentity)
    ));
    drop(proxied);
    drop(direct);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
}

async fn exercise_watermarks_and_coverage(
    a: &RevisionedSmStorage,
    b: &RevisionedSmStorage,
    db: &Database,
    principal: &AuthenticatedPrincipalRef,
) {
    for (label, last) in [("normal", 0_u32), ("wrapped", u32::MAX - 1)] {
        let mut source = session(&format!("revision-gap-{label}"));
        source.last_acked = last;
        source.outbound_count = last.wrapping_add(3);
        source.replay_gap_through = Some(last.wrapping_add(2));
        let frames = vec![frame(&source.stream_id, source.outbound_count)];

        let mut missing_prefix = source.clone();
        missing_prefix.replay_gap_through = None;
        let bad = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Create {
                generation: Uuid::new_v4(),
                principal,
                session: &missing_prefix,
                unacked: &frames,
                drained: &[],
            },
        };
        assert!(matches!(
            a.apply(&bad).await,
            Err(RevisionError::InvalidInput)
        ));

        let mut internal_hole = source.clone();
        internal_hole.outbound_count = last.wrapping_add(4);
        internal_hole.replay_gap_through = Some(last.wrapping_add(1));
        let hole_frames = vec![
            frame(&source.stream_id, last.wrapping_add(2)),
            frame(&source.stream_id, last.wrapping_add(4)),
        ];
        let bad = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Create {
                generation: Uuid::new_v4(),
                principal,
                session: &internal_hole,
                unacked: &hole_frames,
                drained: &[],
            },
        };
        assert!(matches!(
            a.apply(&bad).await,
            Err(RevisionError::InvalidInput)
        ));

        let valid = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Create {
                generation: Uuid::new_v4(),
                principal,
                session: &source,
                unacked: &frames,
                drained: &[],
            },
        };
        a.apply(&valid)
            .await
            .expect("create complete gapped suffix");
        let captured = b
            .capture(&source.stream_id)
            .await
            .expect("capture valid suffix");
        for regressed_gap in [None, Some(last.wrapping_add(1))] {
            let mut proposed = captured.session.clone();
            proposed.replay_gap_through = regressed_gap;
            let snapshot = Request {
                id: Uuid::new_v4(),
                mutation: Mutation::Snapshot {
                    revision: &captured.revision,
                    session: &proposed,
                    unacked: &captured.unacked,
                    drained: &[],
                },
            };
            assert!(matches!(
                a.apply(&snapshot).await,
                Err(RevisionError::InvalidInput)
            ));
            proposed.outbound_count = proposed.outbound_count.wrapping_add(1);
            let append = PersistedDetachedAppend {
                stanza: frame(&source.stream_id, proposed.outbound_count),
                session: proposed,
                evicted_sequences: Vec::new(),
                ingress_append: None,
                new_delivery: false,
            };
            let request = Request {
                id: Uuid::new_v4(),
                mutation: Mutation::Append {
                    revision: &captured.revision,
                    append: &append,
                },
            };
            assert!(matches!(
                a.apply(&request).await,
                Err(RevisionError::InvalidInput)
            ));
        }
        let mut next = captured.session.clone();
        next.outbound_count = next.outbound_count.wrapping_add(1);
        let append = PersistedDetachedAppend {
            stanza: frame(&source.stream_id, next.outbound_count),
            session: next,
            evicted_sequences: Vec::new(),
            ingress_append: None,
            new_delivery: false,
        };
        let request = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Append {
                revision: &captured.revision,
                append: &append,
            },
        };
        b.apply(&request).await.expect("append preserves gap");
        let captured = a
            .capture(&source.stream_id)
            .await
            .expect("capture appended suffix");
        assert_eq!(
            captured.session.replay_gap_through,
            source.replay_gap_through
        );
        let partial_ack = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Acknowledge {
                revision: &captured.revision,
                h: last.wrapping_add(1),
                mode: SmAcknowledgementMode::PreserveResumability,
            },
        };
        a.apply(&partial_ack).await.expect("partial ACK");
        let captured = b
            .capture(&source.stream_id)
            .await
            .expect("capture partial ACK");
        assert_eq!(
            captured.session.replay_gap_through,
            source.replay_gap_through
        );
        let covering_ack = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Acknowledge {
                revision: &captured.revision,
                h: last.wrapping_add(2),
                mode: SmAcknowledgementMode::PreserveResumability,
            },
        };
        b.apply(&covering_ack).await.expect("covering ACK");
        let captured = a
            .capture(&source.stream_id)
            .await
            .expect("gap cleared by ACK");
        assert_eq!(captured.session.replay_gap_through, None);
        assert_eq!(captured.unacked.len(), 2);

        // Simulate a storage corruption after a valid commit: an internal hole
        // must make a fresh capture fail rather than mint a usable revision.
        let mut tx = db.begin_immediate().await.expect("corruption fixture");
        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence = ?",
            waddle_server::db_params![source.stream_id.as_str(), i64::from(source.outbound_count)],
        )
        .await
        .expect("remove retained suffix frame");
        tx.commit().await.expect("commit fixture corruption");
        assert!(matches!(
            b.capture(&source.stream_id).await,
            Err(RevisionError::InvalidInput)
        ));
    }

    let source = session("revision-resume-ceiling");
    let frames = vec![frame(&source.stream_id, 1)];
    let create = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal,
            session: &source,
            unacked: &frames,
            drained: &[],
        },
    };
    a.apply(&create)
        .await
        .expect("create resume ceiling fixture");
    let captured = b
        .capture(&source.stream_id)
        .await
        .expect("capture resume ceiling");
    let retire = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Acknowledge {
            revision: &captured.revision,
            h: 0,
            mode: SmAcknowledgementMode::RetireForPromotion,
        },
    };
    a.apply(&retire).await.expect("retire for promotion");
    let captured = b
        .capture(&source.stream_id)
        .await
        .expect("capture retired source");
    assert_eq!(captured.session.max_resume_time, Some(0));
    assert_eq!(captured.session.max_resume_duration, Duration::ZERO);
    for (resume_time, resume_duration) in [
        (Some(60), Duration::ZERO),
        (Some(0), Duration::from_secs(60)),
        (None, Duration::ZERO),
    ] {
        let mut resurrected = captured.session.clone();
        resurrected.max_resume_time = resume_time;
        resurrected.max_resume_duration = resume_duration;
        let snapshot = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Snapshot {
                revision: &captured.revision,
                session: &resurrected,
                unacked: &captured.unacked,
                drained: &[],
            },
        };
        assert!(matches!(
            a.apply(&snapshot).await,
            Err(RevisionError::InvalidInput)
        ));
        resurrected.outbound_count += 1;
        let append = PersistedDetachedAppend {
            stanza: frame(&source.stream_id, resurrected.outbound_count),
            session: resurrected,
            evicted_sequences: Vec::new(),
            ingress_append: None,
            new_delivery: false,
        };
        let append = Request {
            id: Uuid::new_v4(),
            mutation: Mutation::Append {
                revision: &captured.revision,
                append: &append,
            },
        };
        assert!(matches!(
            a.apply(&append).await,
            Err(RevisionError::InvalidInput)
        ));
    }
    let current = b
        .capture(&source.stream_id)
        .await
        .expect("retired source unchanged");
    assert_eq!(current.session.max_resume_time, Some(0));
    assert_eq!(current.session.max_resume_duration, Duration::ZERO);

    // Coverage work is proportional to supplied rows, not the counter range.
    let mut huge = session("revision-huge-window");
    huge.outbound_count = (1_u32 << 31) - 1;
    let request = Request {
        id: Uuid::new_v4(),
        mutation: Mutation::Create {
            generation: Uuid::new_v4(),
            principal,
            session: &huge,
            unacked: &[],
            drained: &[],
        },
    };
    assert!(matches!(
        a.apply(&request).await,
        Err(RevisionError::InvalidInput)
    ));
}
