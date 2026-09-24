//! A delayed archive lookup must not mutate a later claim of the same row.

use super::*;
use waddle_xmpp::pending_delivery::storage::PendingClaim;

#[derive(Clone, Copy, Debug)]
enum LookupCompletion {
    TransientFailure,
    MissingArchive,
}

#[derive(Clone, Copy, Debug)]
enum ReplacementState {
    Unoffered,
    Offered,
    Sequenced,
}

struct PausedArchiveResolver {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
    completion: LookupCompletion,
}

#[async_trait]
impl ArchiveResolver for PausedArchiveResolver {
    async fn resolve(&self, _id: &StanzaId) -> Result<Option<Message>, ArchiveResolveError> {
        self.entered.notify_one();
        self.resume.notified().await;
        match self.completion {
            LookupCompletion::TransientFailure => Err(ArchiveResolveError::Storage(
                MamStorageError::Database("delayed archive read failed".to_owned()),
            )),
            LookupCompletion::MissingArchive => Ok(None),
        }
    }
}

async fn stale_lookup_cannot_mutate_replacement(
    storage: &Arc<DatabasePendingDeliveryStorage>,
    replacement: &DatabasePendingDeliveryStorage,
    completion: LookupCompletion,
    replacement_state: ReplacementState,
) {
    let recipient = bare("alice@example.com");
    let resource = full("alice@example.com/resumed");
    let session = SmSessionId::new("same-live-stream");
    let pending = archived_row("alice@example.com", "first");
    let row_id = pending.id.clone();
    storage
        .insert(pending)
        .await
        .expect("seed archived pending row");
    let registry = Arc::new(ConnectionRegistry::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let owner = registry.register(resource.clone(), tx);
    registry.update_presence(&resource, true, 0);
    let resolver = Arc::new(PausedArchiveResolver {
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
        completion,
    });
    let old_producer = {
        let storage: Arc<dyn PendingDeliveryStorage> = storage.clone();
        let registry = registry.clone();
        let resolver = resolver.clone();
        let resource = resource.clone();
        let session = session.clone();
        tokio::spawn(async move {
            flush_for_resource(
                &storage,
                &registry,
                &resource.to_bare(),
                &resource,
                FlushContext {
                    server_domain: "example.com",
                    sm_session: Some(&session),
                    blocking_storage: None,
                    owner: Some(&owner),
                    archive_resolver: resolver.as_ref(),
                    dispatch_gate: Some(&FixedPendingDispatch(PendingDispatchReadiness::Ready)),
                },
            )
            .await
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        resolver.entered.notified(),
    )
    .await
    .expect("old producer claimed the row and entered the archive resolver");
    let original = replacement
        .list_unoffered_claims(&recipient, None, i64::MAX, 8)
        .await
        .expect("original durable unoffered claim");
    assert_eq!(original.len(), 1);
    assert_eq!(original[0].row_id, row_id);
    assert_eq!(original[0].session, session);
    assert_eq!(
        replacement
            .release_unpushed_row_if_session(&row_id, &session, &original[0].token)
            .await
            .expect("periodic recovery releases the interrupted claim"),
        1
    );
    let new_token = PendingClaimToken::fresh();
    let claimed = replacement
        .claim_archive_ordered_batch_for_session(&recipient, &session, &new_token, 1)
        .await
        .expect("same SM stream acquires a fresh claim on another storage handle");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, row_id);
    let new_claim = PendingClaim {
        row_id: row_id.clone(),
        session: session.clone(),
        token: new_token,
    };
    let offered = !matches!(replacement_state, ReplacementState::Unoffered);
    if offered {
        assert!(replacement
            .mark_claim_offered(&new_claim)
            .await
            .expect("replacement reserves its offer"));
    }
    let sequence = if matches!(replacement_state, ReplacementState::Sequenced) {
        assert_eq!(
            replacement
                .record_pushed_at(&row_id, 7)
                .await
                .expect("replacement writer stamps its sequence"),
            1
        );
        Some(7)
    } else {
        None
    };

    resolver.resume.notify_one();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), old_producer)
        .await
        .expect("stale resolver completion finishes without waiting for the replacement")
        .expect("old producer task");
    assert_eq!(outcome.pushed, 0, "{completion:?}/{replacement_state:?}");
    assert!(
        rx.try_recv().is_err(),
        "stale producer must not enqueue a replacement's row"
    );
    let rows = replacement
        .list(&recipient)
        .await
        .expect("replacement's durable row survives stale completion");
    assert_eq!(rows.len(), 1, "{completion:?}/{replacement_state:?}");
    assert_eq!(rows[0].id, row_id);
    assert_eq!(
        rows[0].flushed_in_session.as_ref(),
        Some(&session),
        "{completion:?}/{replacement_state:?}"
    );
    assert_eq!(
        rows[0].outbound_sequence, sequence,
        "{completion:?}/{replacement_state:?}"
    );
    let db = replacement.database();
    let guard = db.guard().await.expect("inspect exact durable claim fence");
    let mut matches = guard.query(
        "SELECT COUNT(*) FROM pending_delivery WHERE row_id = ? AND claim_token = ? AND claim_offered = ?",
        crate::db_params![row_id.as_str(), new_token.to_string(), i64::from(offered)],
    ).await.expect("replacement token and offer marker");
    let count: i64 = matches
        .next()
        .await
        .expect("count row")
        .expect("count")
        .get(0)
        .expect("matching claims");
    assert_eq!(
        count, 1,
        "stale {completion:?} must preserve {replacement_state:?} token/offer custody"
    );
    drop(matches);
    drop(guard);
    replacement
        .delete_row(&row_id)
        .await
        .expect("clean isolated scenario");
}

async fn check_stale_archive_cleanup(database_url: &str) {
    let storage = Arc::new(ordered_pending_fixture(Some(database_url)).await);
    // Reuse the fixture's real archive-order schema, retaining only the one
    // scenario row at a time so each old resolver pauses on that exact row.
    for row in storage
        .list(&bare("alice@example.com"))
        .await
        .expect("fixture rows")
    {
        storage
            .delete_row(&row.id)
            .await
            .expect("clear fixture backlog");
    }
    let replacement =
        DatabasePendingDeliveryStorage::open(Some(database_url), QuotaPolicy::Unlimited)
            .await
            .expect("independent replacement storage handle");
    for completion in [
        LookupCompletion::TransientFailure,
        LookupCompletion::MissingArchive,
    ] {
        for state in [
            ReplacementState::Unoffered,
            ReplacementState::Offered,
            ReplacementState::Sequenced,
        ] {
            stale_lookup_cannot_mutate_replacement(&storage, &replacement, completion, state).await;
        }
    }
}

#[tokio::test]
async fn sqlite_stale_archive_completion_preserves_same_stream_replacement_claims() {
    let directory = tempfile::tempdir().expect("stale cleanup database directory");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("cleanup.db").display()
    );
    check_stale_archive_cleanup(&database_url).await;
}

#[tokio::test]
async fn postgres_stale_archive_completion_preserves_same_stream_replacement_claims() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (stale pending cleanup)");
        return;
    };
    let (schema, scoped_url) =
        create_postgres_test_schema(&database_url, "pending_stale_cleanup").await;
    check_stale_archive_cleanup(&scoped_url).await;
    drop_postgres_test_schema(&database_url, &schema).await;
}
