#[cfg(feature = "clustering")]
use chrono::{TimeZone, Utc};
#[cfg(feature = "clustering")]
use sqlx::Connection;
use uuid::Uuid;
#[cfg(feature = "clustering")]
use waddle_xmpp::ingress::{EffectMessageIdentity, InboxProjectionMutation, IngressEffectIntent};
use waddle_xmpp::ingress::{MessageKey, ProtocolEpoch, SemanticDigest, SmIngressId};
#[cfg(feature = "clustering")]
use waddle_xmpp::{
    auth::{AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch},
    ingress::{AliasOutcome, AliasResolution, DeliveryKey, IngressOrdinal, NormalizedTarget},
    ownership::{ClaimEpoch, ClaimStore, EntityType, NodeIdentity, SharedNodeIdentity},
    pending_delivery::SmSessionId,
};
#[cfg(feature = "clustering")]
use waddle_xmpp::{
    inbox::{
        storage::{GroupchatNotificationRecovery, GroupchatNotificationRecoveryKey},
        ConversationKind, InboxEntry,
    },
    mam::{ArchivedMessage, MamTxStoreOutcome},
};
#[cfg(feature = "clustering")]
use waddle_xmpp_core::xep0359::OriginId;
#[cfg(feature = "clustering")]
use waddle_xmpp_core::xep0359::StanzaId;
#[cfg(feature = "clustering")]
use xmpp_parsers::message::MessageType;

use super::{CanonicalMessageRepository, IngressUowError, PostgresIngressUnitOfWork};
#[cfg(feature = "clustering")]
use super::{
    ClaimRepository, DeliveryEffectRepository, EffectIntentRepository, EffectIntentWriteOutcome,
    HandledFrontierOutcome, HandledFrontierRepository, InboxRepository, IngressUowTransaction,
    MamArchiveRepository, PrincipalAssertion, PrincipalRepository, ShadowFrontierOutcome,
    SmIngressRepository, SmIngressStreamRepository,
};
use crate::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseConfig, DatabaseDriver, IntoParams, MigrationRunner},
};

#[tokio::test]
async fn open_rejects_sqlite() {
    let db = Database::in_memory("ingress_uow_sqlite")
        .await
        .expect("open sqlite database");
    assert!(matches!(
        PostgresIngressUnitOfWork::open(db, LineageConfig::default()),
        Err(IngressUowError::PostgresRequired)
    ));
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn spanning_proof_commits_exact_cross_store_values() {
    let Some(fixture) = Fixture::open("spanning_proof").await else {
        return;
    };
    let values = FixtureValues::new("spanning-proof");
    fixture.seed_claim(&values).await;

    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("exact claim mints fence");
    insert_session_in_uow(&mut transaction, &values, 0).await;
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 1)
            .await
            .expect("advance handled frontier"),
        HandledFrontierOutcome::Advanced
    );
    write_spanning_rows(&mut transaction, &values).await;
    transaction.commit().await.expect("commit spanning proof");

    fixture.assert_spanning_rows(&values, 1).await;

    let mut retry = archived_message(&values);
    retry.id = format!("retry-{}", values.mam_id);
    let mut retry_transaction = fixture.begin().await;
    match MamArchiveRepository::store(&mut retry_transaction, &values.archive_jid, &retry)
        .await
        .expect("deduplicate MAM retry")
    {
        MamTxStoreOutcome::Existing(stanza_id) => {
            assert_eq!(stanza_id.id, values.mam_id);
            assert_eq!(stanza_id.by, jid::Jid::from(values.archive_jid.clone()));
        }
        outcome => panic!("expected existing MAM archive row, got {outcome:?}"),
    }
    retry_transaction
        .commit()
        .await
        .expect("commit MAM dedup proof");
    assert_eq!(fixture.count("mam_messages").await, 1);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn dropping_uow_rolls_back_spanning_writes() {
    let Some(fixture) = Fixture::open("atomicity").await else {
        return;
    };
    let values = FixtureValues::new("atomicity");
    fixture.seed_claim(&values).await;

    {
        let mut transaction = fixture.begin().await;
        let fence = ClaimRepository::assert_sm_claim(
            &mut transaction,
            &values.stream_id,
            &values.owner,
            values.claim_epoch,
        )
        .await
        .expect("exact claim mints fence");
        insert_session_in_uow(&mut transaction, &values, 0).await;
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 1)
            .await
            .expect("advance handled frontier");
        write_spanning_rows(&mut transaction, &values).await;
    }

    fixture.assert_spanning_rows(&values, 0).await;
    fixture.assert_session_absent(&values.stream_id).await;
    fixture.close().await;
}

#[tokio::test]
async fn epoch_one_uow_write_succeeds_and_raw_write_is_rejected() {
    let Some(fixture) = Fixture::open("guard_interaction").await else {
        return;
    };
    fixture.advance_epoch_to_one().await;
    let uow = fixture.uow();
    let mut transaction = uow.begin().await.expect("begin epoch-one uow");
    let message_key = MessageKey::new();
    CanonicalMessageRepository::record(&mut transaction, message_key, &digest(1))
        .await
        .expect("UoW carries epoch proof");
    #[cfg(feature = "clustering")]
    SmIngressStreamRepository::mint(
        &mut transaction,
        &SmSessionId::new(format!("uow-guard-stream-{}", Uuid::new_v4().simple())),
    )
    .await
    .expect("UoW can enroll a guarded SM stream");
    #[cfg(feature = "clustering")]
    assert_eq!(
        EffectIntentRepository::record_all(
            &mut transaction,
            message_key,
            &[IngressEffectIntent::InboxProject {
                owner: "romeo@example.com".parse().expect("valid fixture JID"),
                mutation: InboxProjectionMutation::Direct {
                    peer: "juliet@example.com".parse().expect("valid fixture JID"),
                    increment_unread: true,
                },
            }],
        )
        .await
        .expect("UoW can record guarded effect intents"),
        EffectIntentWriteOutcome::Recorded
    );
    transaction.commit().await.expect("commit UoW write");

    let raw = fixture
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, ?, ?)",
            crate::db_params![MessageKey::new().to_storage().to_string(), 1_i64, vec![2_u8; 32]],
        )
        .await;
    assert!(raw.is_err(), "the V1009 trigger rejects unproven writes");
    let raw_stream = fixture
        .execute(
            "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?::uuid, ?)",
            crate::db_params![
                SmIngressId::new().to_storage().to_string(),
                "raw-guard-stream".to_string()
            ],
        )
        .await;
    assert!(
        raw_stream.is_err(),
        "the V1010 stream guard rejects unproven writes"
    );
    let raw_intent = fixture
        .execute(
            "INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?::uuid, 0, 0, ?, 1, ?)",
            crate::db_params![
                message_key.to_storage().to_string(),
                vec![0_u8; 32],
                vec![1_u8]
            ],
        )
        .await;
    assert!(
        raw_intent.is_err(),
        "the V1010 effect-intent guard rejects unproven writes"
    );
    fixture.close().await;
}

#[tokio::test]
async fn lineage_mismatch_and_missing_row_fail_closed() {
    let Some(fixture) = Fixture::open("lineage_negatives").await else {
        return;
    };
    let mismatch = LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90002"
                .parse()
                .expect("valid mismatch deployment UUID"),
        ),
        action: None,
    };
    let mismatch_uow =
        PostgresIngressUnitOfWork::open(fixture.db.clone(), mismatch).expect("open mismatch UoW");
    assert!(matches!(
        mismatch_uow.begin().await,
        Err(IngressUowError::Lineage(crate::db::DatabaseError::Lineage(
            lineage::LineageError::DeploymentUuidMismatch { .. }
        )))
    ));

    fixture
        .execute("DELETE FROM _lineage", ())
        .await
        .expect("delete lineage row");
    let uow = fixture.uow();
    assert!(matches!(
        uow.begin().await,
        Err(IngressUowError::Lineage(crate::db::DatabaseError::Lineage(
            lineage::LineageError::MissingRow
        )))
    ));
    fixture.close().await;
}

#[tokio::test]
async fn epoch_fence_accepts_live_zero_and_one_and_rejects_future_epoch() {
    let Some(fixture) = Fixture::open("epoch_fence").await else {
        return;
    };
    let transaction = fixture.begin().await;
    assert_eq!(transaction.protocol_epoch(), ProtocolEpoch::ZERO);
    transaction.commit().await.expect("commit epoch-zero proof");

    fixture.advance_epoch_to_one().await;
    let transaction = fixture.begin().await;
    assert_eq!(transaction.protocol_epoch(), ProtocolEpoch::from_storage(1));
    transaction.commit().await.expect("commit epoch-one proof");

    fixture.set_epoch(2).await;
    let uow = fixture.uow();
    assert!(matches!(
        uow.begin().await,
        Err(IngressUowError::EpochUnsupported {
            live,
            supported: _
        }) if live == ProtocolEpoch::from_storage(2)
    ));
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn claim_fence_requires_the_exact_owner_incarnation_and_epoch() {
    let Some(fixture) = Fixture::open("claim_fence_negatives").await else {
        return;
    };
    let values = FixtureValues::new("claim-fence");
    fixture.seed_claim_and_session(&values, 0).await;
    let absent = SmSessionId::new("missing-stream");
    let wrong_owner = NodeIdentity::new("other-node", values.owner.node_epoch.clone());
    let wrong_incarnation = NodeIdentity::new(values.owner.node_id.clone(), "other-epoch");

    for (stream_id, owner, claim_epoch) in [
        (&absent, &values.owner, values.claim_epoch),
        (&values.stream_id, &wrong_owner, values.claim_epoch),
        (&values.stream_id, &wrong_incarnation, values.claim_epoch),
        (
            &values.stream_id,
            &values.owner,
            ClaimEpoch(values.claim_epoch.0 + 1),
        ),
    ] {
        let mut transaction = fixture.begin().await;
        assert!(matches!(
            ClaimRepository::assert_sm_claim(&mut transaction, stream_id, owner, claim_epoch).await,
            Err(IngressUowError::ClaimFenceMissing)
        ));
    }
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn claim_fence_blocks_a_concurrent_claim_update_until_commit() {
    let Some(fixture) = Fixture::open("claim_fence_concurrency").await else {
        return;
    };
    let values = FixtureValues::new("claim-fence-concurrency");
    fixture.seed_claim_and_session(&values, 0).await;
    let mut transaction = fixture.begin().await;
    let _fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("hold exact claim fence");

    let schema_url = fixture.schema_url.clone();
    let entity = sm_claim_entity(&values.stream_id);
    let update = tokio::spawn(async move {
        let mut connection = sqlx::PgConnection::connect(&schema_url)
            .await
            .expect("open competing claim connection");
        sqlx::query("UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = $1")
            .bind(entity)
            .execute(&mut connection)
            .await
            .expect("claim update completes after fence release");
    });
    wait_for_lock_waiter(&fixture.admin, "UPDATE clustering_claims SET claim_epoch").await;
    assert!(!update.is_finished(), "claim update must still be waiting");
    transaction.commit().await.expect("release claim fence");
    update.await.expect("join competing claim update");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn claim_fence_requires_current_local_node_authority() {
    let Some(fixture) = Fixture::open("claim_fence_authority").await else {
        return;
    };
    let values = FixtureValues::new("claim-fence-authority");
    fixture.seed_claim_and_session(&values, 0).await;

    // Rotation of the bound canonical identity to a new incarnation revokes
    // minting under the old identity even though the old clustering_claims
    // row still exists.
    fixture
        .node_identity
        .rotate(NodeIdentity::new(
            values.owner.node_id.clone(),
            "rotated-epoch",
        ))
        .await;
    let mut transaction = fixture.begin().await;
    assert!(matches!(
        ClaimRepository::assert_sm_claim(
            &mut transaction,
            &values.stream_id,
            &values.owner,
            values.claim_epoch
        )
        .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(transaction);

    // Terminal disable revokes minting the same way.
    fixture.node_identity.rotate(values.owner.clone()).await;
    fixture.node_identity.disable().await;
    let mut transaction = fixture.begin().await;
    assert!(matches!(
        ClaimRepository::assert_sm_claim(
            &mut transaction,
            &values.stream_id,
            &values.owner,
            values.claim_epoch
        )
        .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(transaction);

    // A unit of work with no bound canonical identity cannot mint at all.
    let unbound = PostgresIngressUnitOfWork::open(fixture.db.clone(), fixture.lineage.clone())
        .expect("open unbound unit of work");
    let mut transaction = unbound.begin().await.expect("begin unbound transaction");
    assert!(matches!(
        ClaimRepository::assert_sm_claim(
            &mut transaction,
            &values.stream_id,
            &values.owner,
            values.claim_epoch
        )
        .await,
        Err(IngressUowError::NodeIdentityUnbound)
    ));
    drop(transaction);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn claim_fence_from_another_live_transaction_is_rejected() {
    let Some(fixture) = Fixture::open("claim_fence_replay").await else {
        return;
    };
    let values = FixtureValues::new("claim-fence-replay");
    fixture.seed_claim_and_session(&values, 0).await;

    let mut minting_transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_sm_claim(
        &mut minting_transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint fence in the first live transaction");

    // A fence minted by one live transaction must not authorize fenced
    // writes in another live transaction, even for the same stream.
    let mut other_transaction = fixture.begin().await;
    assert!(matches!(
        HandledFrontierRepository::advance(&mut other_transaction, &fence, &values.stream_id, 1)
            .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(other_transaction);

    // The same fence keeps working in the transaction that minted it.
    assert_eq!(
        HandledFrontierRepository::advance(&mut minting_transaction, &fence, &values.stream_id, 1)
            .await
            .expect("advance in the minting transaction"),
        HandledFrontierOutcome::Advanced
    );
    minting_transaction
        .commit()
        .await
        .expect("commit minting transaction");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn room_claim_fence_authorizes_a_transaction_bound_mam_archive_write() {
    let Some(fixture) = Fixture::open("room_claim_fence").await else {
        return;
    };
    let values = FixtureValues::new("room-claim-fence");
    fixture.seed_room_claim(&values).await;

    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_room_claim(
        &mut transaction,
        &values.archive_jid,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("exact room claim mints fence");
    match MamArchiveRepository::store_fenced(
        &mut transaction,
        &fence,
        &values.archive_jid,
        &archived_message(&values),
    )
    .await
    .expect("store under room claim fence")
    {
        MamTxStoreOutcome::Inserted(stanza_id) => {
            assert_eq!(stanza_id.id, values.mam_id);
            assert_eq!(stanza_id.by, jid::Jid::from(values.archive_jid.clone()));
        }
        outcome => panic!("expected inserted MAM archive row, got {outcome:?}"),
    }
    transaction.commit().await.expect("commit room archive");
    assert_eq!(fixture.count("mam_messages").await, 1);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn room_claim_fence_rejects_a_different_archive_room() {
    let Some(fixture) = Fixture::open("room_claim_wrong_room").await else {
        return;
    };
    let values = FixtureValues::new("room-claim-wrong-room");
    fixture.seed_room_claim(&values).await;
    let wrong_archive: jid::BareJid = "wrong-room@conference.example.com"
        .parse()
        .expect("valid wrong-room archive JID");

    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_room_claim(
        &mut transaction,
        &values.archive_jid,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint room fence");
    assert!(matches!(
        MamArchiveRepository::store_fenced(
            &mut transaction,
            &fence,
            &wrong_archive,
            &archived_message(&values),
        )
        .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    transaction
        .commit()
        .await
        .expect("commit rejected room write");
    assert_eq!(fixture.count("mam_messages").await, 0);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn room_claim_fence_requires_a_persisted_room_claim() {
    let Some(fixture) = Fixture::open("room_claim_missing").await else {
        return;
    };
    let values = FixtureValues::new("room-claim-missing");
    let mut transaction = fixture.begin().await;
    assert!(matches!(
        ClaimRepository::assert_room_claim(
            &mut transaction,
            &values.archive_jid,
            &values.owner,
            values.claim_epoch,
        )
        .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(transaction);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn room_claim_fence_requires_a_bound_node_identity() {
    let Some(fixture) = Fixture::open("room_claim_unbound").await else {
        return;
    };
    let values = FixtureValues::new("room-claim-unbound");
    fixture.seed_room_claim(&values).await;
    let unbound = PostgresIngressUnitOfWork::open(fixture.db.clone(), fixture.lineage.clone())
        .expect("open unbound unit of work");
    let mut transaction = unbound.begin().await.expect("begin unbound transaction");
    assert!(matches!(
        ClaimRepository::assert_room_claim(
            &mut transaction,
            &values.archive_jid,
            &values.owner,
            values.claim_epoch,
        )
        .await,
        Err(IngressUowError::NodeIdentityUnbound)
    ));
    drop(transaction);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn room_claim_fence_from_another_live_transaction_is_rejected() {
    let Some(fixture) = Fixture::open("room_claim_replay").await else {
        return;
    };
    let values = FixtureValues::new("room-claim-replay");
    fixture.seed_room_claim(&values).await;

    let mut minting_transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_room_claim(
        &mut minting_transaction,
        &values.archive_jid,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint room fence in first transaction");
    let mut other_transaction = fixture.begin().await;
    assert!(matches!(
        MamArchiveRepository::store_fenced(
            &mut other_transaction,
            &fence,
            &values.archive_jid,
            &archived_message(&values),
        )
        .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(other_transaction);
    assert!(matches!(
        MamArchiveRepository::store_fenced(
            &mut minting_transaction,
            &fence,
            &values.archive_jid,
            &archived_message(&values),
        )
        .await
        .expect("store in minting transaction"),
        MamTxStoreOutcome::Inserted(_)
    ));
    minting_transaction
        .commit()
        .await
        .expect("commit minting transaction");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn handled_frontier_uses_wrapping_single_step_cas() {
    let Some(fixture) = Fixture::open("frontier_cas").await else {
        return;
    };
    let values = FixtureValues::new("frontier-cas");
    fixture.seed_claim_and_session(&values, u32::MAX).await;
    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint fence");
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, u32::MAX)
            .await
            .expect("equal frontier is idempotent"),
        HandledFrontierOutcome::Idempotent
    );
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 0)
            .await
            .expect("wrapping frontier advances"),
        HandledFrontierOutcome::Advanced
    );
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 1)
            .await
            .expect("next frontier advances"),
        HandledFrontierOutcome::Advanced
    );
    assert!(matches!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 3).await,
        Err(IngressUowError::FrontierStale {
            stored: 1,
            offered: 3
        })
    ));
    let missing = SmSessionId::new("missing-frontier-stream");
    assert!(matches!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &missing, 1).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    transaction
        .commit()
        .await
        .expect("commit frontier outcomes");

    let mut missing_transaction = fixture.begin().await;
    let missing_fence = ClaimRepository::assert_sm_claim(
        &mut missing_transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint fence for missing stream proof");
    fixture.delete_session(&values.stream_id).await;
    assert!(matches!(
        HandledFrontierRepository::advance(
            &mut missing_transaction,
            &missing_fence,
            &values.stream_id,
            1,
        )
        .await,
        Err(IngressUowError::StreamMissing)
    ));
    drop(missing_transaction);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn shadow_stream_mint_is_idempotent_per_stream_and_unique_across_streams() {
    let Some(fixture) = Fixture::open("shadow_stream_mint").await else {
        return;
    };
    let first_stream = SmSessionId::new(format!("mint-a-{}", Uuid::new_v4().simple()));
    let second_stream = SmSessionId::new(format!("mint-b-{}", Uuid::new_v4().simple()));

    let mut first = fixture.begin().await;
    let first_id = SmIngressStreamRepository::mint(&mut first, &first_stream)
        .await
        .expect("mint first stream");
    first.commit().await.expect("commit first mint");

    let mut retry = fixture.begin().await;
    let repeated_id = SmIngressStreamRepository::mint(&mut retry, &first_stream)
        .await
        .expect("idempotent mint");
    let second_id = SmIngressStreamRepository::mint(&mut retry, &second_stream)
        .await
        .expect("mint distinct stream");
    retry.commit().await.expect("commit repeated mints");

    assert_eq!(repeated_id, first_id, "same stream keeps its ingress ID");
    assert_ne!(
        second_id, first_id,
        "distinct streams have distinct ingress IDs"
    );
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn shadow_stream_lock_requires_the_current_transaction_fence() {
    let Some(fixture) = Fixture::open("shadow_stream_lock").await else {
        return;
    };
    let values = FixtureValues::new("shadow-stream-lock");
    fixture.seed_claim(&values).await;

    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint exact claim fence");
    assert_eq!(
        SmIngressStreamRepository::lock(&mut transaction, &fence, &values.stream_id)
            .await
            .expect("missing enrollment is not an error"),
        None
    );
    let enrolled = SmIngressStreamRepository::mint(&mut transaction, &values.stream_id)
        .await
        .expect("enroll stream");
    assert_eq!(
        SmIngressStreamRepository::lock(&mut transaction, &fence, &values.stream_id)
            .await
            .expect("lock enrolled stream"),
        Some((enrolled, 0))
    );

    let mut other = fixture.begin().await;
    assert!(matches!(
        SmIngressStreamRepository::lock(&mut other, &fence, &values.stream_id).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(other);
    transaction
        .commit()
        .await
        .expect("commit stream enrollment");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn shadow_frontier_advances_idempotently_and_detects_gaps() {
    let Some(fixture) = Fixture::open("shadow_frontier").await else {
        return;
    };
    let values = FixtureValues::new("shadow-frontier");
    fixture.seed_claim(&values).await;
    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint exact claim fence");
    let stream_id = SmIngressStreamRepository::mint(&mut transaction, &values.stream_id)
        .await
        .expect("enroll stream");
    assert_eq!(
        SmIngressStreamRepository::advance_frontier(
            &mut transaction,
            &fence,
            stream_id,
            IngressOrdinal::FIRST,
        )
        .await
        .expect("advance 0 to 1"),
        ShadowFrontierOutcome::Advanced
    );
    assert_eq!(
        SmIngressStreamRepository::advance_frontier(
            &mut transaction,
            &fence,
            stream_id,
            IngressOrdinal::FIRST,
        )
        .await
        .expect("replay is idempotent"),
        ShadowFrontierOutcome::Idempotent
    );
    assert_eq!(
        SmIngressStreamRepository::advance_frontier(
            &mut transaction,
            &fence,
            stream_id,
            IngressOrdinal::from_storage(3).expect("valid ordinal"),
        )
        .await
        .expect("gap is observable"),
        ShadowFrontierOutcome::Stale { stored: 1 }
    );
    assert_eq!(
        SmIngressStreamRepository::advance_frontier(
            &mut transaction,
            &fence,
            stream_id,
            IngressOrdinal::from_storage(2).expect("valid ordinal"),
        )
        .await
        .expect("advance 1 to 2"),
        ShadowFrontierOutcome::Advanced
    );
    transaction.commit().await.expect("commit frontier updates");

    let conn = fixture.db.guard().await.expect("read committed stream row");
    let mut rows = conn
        .query(
            "SELECT handled_ordinal::text, row_revision::text FROM ingress_sm_streams WHERE sm_ingress_id = ?::uuid",
            crate::db_params![stream_id.to_storage().to_string()],
        )
        .await
        .expect("read shadow frontier");
    let row = rows
        .next()
        .await
        .expect("read stream row")
        .expect("stream row exists");
    assert_eq!(row.get::<String>(0).expect("decode frontier"), "2");
    assert_eq!(row.get::<String>(1).expect("decode revision"), "2");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn every_effect_intent_kind_round_trips_through_postgres_storage() {
    let Some(fixture) = Fixture::open("every_effect_intent_kind").await else {
        return;
    };
    let message_key = MessageKey::new();
    let intents = IngressEffectIntent::storage_round_trip_samples();
    let expected_kinds = intents
        .iter()
        .map(|intent| {
            intent
                .with_encoded_v1(|kind, _| kind)
                .expect("encode representative intent")
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut transaction = fixture.begin().await;
    CanonicalMessageRepository::record(&mut transaction, message_key, &digest(10))
        .await
        .expect("record effect parent message");
    assert_eq!(
        EffectIntentRepository::record_all(&mut transaction, message_key, &intents)
            .await
            .expect("persist every codec kind"),
        EffectIntentWriteOutcome::Recorded
    );
    transaction.commit().await.expect("commit effect intents");

    let conn = fixture.db.guard().await.expect("read stored effects");
    let mut rows = conn
        .query(
            "SELECT kind::int, payload FROM ingress_effect_intents WHERE message_key = ?::uuid ORDER BY effect_ordinal",
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .expect("select stored effects");
    let mut decoded_kinds = std::collections::BTreeSet::new();
    while let Some(row) = rows.next().await.expect("read stored effect") {
        let kind = i32::try_from(row.get::<i64>(0).expect("effect kind")).expect("i32 kind");
        let payload = row.get::<Vec<u8>>(1).expect("effect payload");
        IngressEffectIntent::decode_v1(kind, &payload).expect("decode persisted effect");
        decoded_kinds.insert(kind);
    }
    assert_eq!(decoded_kinds, expected_kinds);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn effect_intents_are_keyed_by_semantic_identity_and_classify_existing_alias_divergence() {
    let Some(fixture) = Fixture::open("effect_intents").await else {
        return;
    };
    let message_key = MessageKey::new();
    let intents = vec![
        IngressEffectIntent::RouteDirect {
            recipient: "juliet@example.com".parse().expect("valid recipient"),
            fanout: vec!["juliet@example.com/phone".parse().expect("valid fanout")],
            route_identity: EffectMessageIdentity::capture_ordinal(0),
        },
        IngressEffectIntent::InboxProject {
            owner: "romeo@example.com".parse().expect("valid owner"),
            mutation: InboxProjectionMutation::Direct {
                peer: "juliet@example.com".parse().expect("valid recipient"),
                increment_unread: true,
            },
        },
        IngressEffectIntent::RouteDirect {
            recipient: "juliet@example.com".parse().expect("valid recipient"),
            fanout: vec!["juliet@example.com/phone".parse().expect("valid fanout")],
            route_identity: EffectMessageIdentity::capture_ordinal(0),
        },
    ];
    let mut transaction = fixture.begin().await;
    CanonicalMessageRepository::record(&mut transaction, message_key, &digest(9))
        .await
        .expect("record effect parent message");
    assert_eq!(
        EffectIntentRepository::record_all(&mut transaction, message_key, &intents)
            .await
            .expect("record effect intents"),
        EffectIntentWriteOutcome::Recorded
    );
    transaction.commit().await.expect("commit intents");

    let conn = fixture.db.guard().await.expect("read effect rows");
    let mut rows = conn
        .query(
            "SELECT effect_ordinal::text, kind::int, octet_length(semantic_identity_hash) FROM ingress_effect_intents WHERE message_key = ?::uuid ORDER BY effect_ordinal",
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .expect("select ordered effects");
    let mut stored = Vec::new();
    while let Some(row) = rows.next().await.expect("read effect row") {
        stored.push((
            row.get::<String>(0).expect("ordinal"),
            row.get::<i64>(1).expect("kind"),
            row.get::<i32>(2).expect("semantic hash length"),
        ));
    }
    assert_eq!(
        stored,
        vec![("0".to_string(), 1, 32), ("1".to_string(), 6, 32)]
    );

    let mut replay = fixture.begin().await;
    assert_eq!(
        EffectIntentRepository::record_all(&mut replay, message_key, &intents)
            .await
            .expect("byte-identical replay"),
        EffectIntentWriteOutcome::AlreadyRecorded
    );
    replay.commit().await.expect("commit replay");

    let differing = [IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("valid recipient"),
        fanout: vec!["juliet@example.com/laptop".parse().expect("valid fanout")],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    }];
    let mut existing_alias_divergence = fixture.begin().await;
    assert_eq!(
        EffectIntentRepository::record_all_existing_alias(
            &mut existing_alias_divergence,
            message_key,
            &differing,
        )
        .await
        .expect("existing-alias divergence should advance"),
        EffectIntentWriteOutcome::IntentDivergence
    );
    existing_alias_divergence
        .commit()
        .await
        .expect("commit existing-alias divergence");
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);

    let subset = [IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("valid recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("valid fanout")],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    }];
    let mut subset_divergence = fixture.begin().await;
    assert_eq!(
        EffectIntentRepository::record_all_existing_alias(
            &mut subset_divergence,
            message_key,
            &subset,
        )
        .await
        .expect("subset divergence should advance"),
        EffectIntentWriteOutcome::IntentDivergence
    );
    subset_divergence
        .commit()
        .await
        .expect("commit subset divergence");
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);

    let mut conflict = fixture.begin().await;
    assert!(matches!(
        EffectIntentRepository::record_all(&mut conflict, message_key, &differing).await,
        Err(IngressUowError::EffectIntentConflict)
    ));
    drop(conflict);

    let empty_message_key = MessageKey::new();
    let mut empty_original = fixture.begin().await;
    CanonicalMessageRepository::record(&mut empty_original, empty_message_key, &digest(10))
        .await
        .expect("record parent with no effect intents");
    empty_original
        .commit()
        .await
        .expect("commit parent with no effect intents");
    let mut empty_existing_alias = fixture.begin().await;
    assert_eq!(
        EffectIntentRepository::record_all_existing_alias(
            &mut empty_existing_alias,
            empty_message_key,
            &subset,
        )
        .await
        .expect("existing alias with new effects must not write"),
        EffectIntentWriteOutcome::IntentDivergence
    );
    empty_existing_alias
        .commit()
        .await
        .expect("commit empty existing-alias divergence");
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);

    let mut missing = fixture.begin().await;
    assert!(matches!(
        EffectIntentRepository::record_all(&mut missing, MessageKey::new(), &intents,).await,
        Err(IngressUowError::EffectIntentMessageMissing)
    ));
    drop(missing);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn principal_assertion_checks_each_persisted_identity_field_and_expiry() {
    let Some(fixture) = Fixture::open("principal_assertion").await else {
        return;
    };
    let values = FixtureValues::new("principal-assertion");
    seed_principal_session(&fixture, &values.principal, None).await;

    let mut happy = fixture.begin().await;
    assert_eq!(
        PrincipalRepository::assert_principal(&mut happy, &values.principal)
            .await
            .expect("assert matching principal"),
        PrincipalAssertion::Asserted
    );
    happy.commit().await.expect("commit happy assertion");

    for principal in [
        AuthenticatedPrincipalRef::new(
            "juliet@example.com"
                .parse()
                .expect("valid mismatched bare JID"),
            values.principal.auth_context_id().clone(),
            values.principal.auth_context_version(),
            values.principal.auth_epoch(),
        ),
        AuthenticatedPrincipalRef::new(
            values.principal.bare_jid().clone(),
            AuthContextId::new(Uuid::new_v4()),
            values.principal.auth_context_version(),
            values.principal.auth_epoch(),
        ),
        AuthenticatedPrincipalRef::new(
            values.principal.bare_jid().clone(),
            values.principal.auth_context_id().clone(),
            AuthContextVersion::new(values.principal.auth_context_version().get() + 1),
            values.principal.auth_epoch(),
        ),
        AuthenticatedPrincipalRef::new(
            values.principal.bare_jid().clone(),
            values.principal.auth_context_id().clone(),
            values.principal.auth_context_version(),
            PrincipalAuthEpoch::new(values.principal.auth_epoch().get() + 1),
        ),
    ] {
        let mut transaction = fixture.begin().await;
        assert_eq!(
            PrincipalRepository::assert_principal(&mut transaction, &principal)
                .await
                .expect("mismatched principal is an outcome"),
            PrincipalAssertion::PrincipalAssertionFailed
        );
        transaction.commit().await.expect("commit failed assertion");
    }

    let expired = AuthenticatedPrincipalRef::new(
        "mercutio@example.com".parse().expect("valid expired JID"),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    );
    seed_principal_session(
        &fixture,
        &expired,
        Some(Utc::now() - chrono::Duration::seconds(1)),
    )
    .await;
    let mut transaction = fixture.begin().await;
    assert_eq!(
        PrincipalRepository::assert_principal(&mut transaction, &expired)
            .await
            .expect("expired principal is an outcome"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    transaction
        .commit()
        .await
        .expect("commit expired assertion");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn principal_assertion_observes_concurrent_session_revocation_after_lock_release() {
    let Some(fixture) = Fixture::open("principal_revocation").await else {
        return;
    };
    let values = FixtureValues::new("principal-revocation");
    let session_id = seed_principal_session(&fixture, &values.principal, None).await;
    let mut connection = sqlx::PgConnection::connect(&fixture.schema_url)
        .await
        .expect("open revocation connection");
    let mut blocker = connection
        .begin()
        .await
        .expect("begin revocation transaction");
    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(&session_id)
        .execute(&mut *blocker)
        .await
        .expect("stage session revocation");

    let uow = fixture.uow();
    let principal = values.principal.clone();
    let assertion = tokio::spawn(async move {
        let mut transaction = uow.begin().await.expect("begin assertion transaction");
        let outcome = PrincipalRepository::assert_principal(&mut transaction, &principal).await;
        drop(transaction);
        outcome
    });
    wait_for_lock_waiter(&fixture.admin, "SELECT expires_at FROM sessions").await;
    assert!(
        !assertion.is_finished(),
        "share assertion waits for deletion lock"
    );
    blocker.commit().await.expect("commit revocation");
    assert_eq!(
        assertion
            .await
            .expect("join assertion task")
            .expect("read revoked principal"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    fixture.close().await;
}

#[cfg(feature = "clustering")]
async fn write_spanning_rows(transaction: &mut IngressUowTransaction<'_>, values: &FixtureValues) {
    store_mam_message(transaction, values).await;
    upsert_inbox_entry(transaction, values).await;
    // The alias miss path mints the canonical message row itself; recording
    // `values.message_key` separately first would collide on the primary key.
    assert!(matches!(
        CanonicalMessageRepository::resolve_and_record_alias(
            transaction,
            &values.sender,
            &values.target,
            &values.origin_id,
            &values.digest,
            || values.message_key,
        )
        .await
        .expect("record origin alias"),
        AliasResolution::Aliased(AliasOutcome::Inserted(key)) if key == values.message_key
    ));
    SmIngressRepository::insert(
        transaction,
        values.sm_ingress_id,
        values.ordinal,
        values.message_key,
    )
    .await
    .expect("record SM ingress reference");
    DeliveryEffectRepository::record(transaction, values.delivery_key, values.message_key)
        .await
        .expect("record delivery identity");
}

#[cfg(feature = "clustering")]
async fn store_mam_message(transaction: &mut IngressUowTransaction<'_>, values: &FixtureValues) {
    match MamArchiveRepository::store(transaction, &values.archive_jid, &archived_message(values))
        .await
        .expect("store MAM identity in UoW")
    {
        MamTxStoreOutcome::Inserted(stanza_id) => {
            assert_eq!(stanza_id.id, values.mam_id);
            assert_eq!(stanza_id.by, jid::Jid::from(values.archive_jid.clone()));
        }
        outcome => panic!("expected MAM identity insertion, got {outcome:?}"),
    }
}

#[cfg(feature = "clustering")]
async fn upsert_inbox_entry(transaction: &mut IngressUowTransaction<'_>, values: &FixtureValues) {
    let entry = InboxEntry::new(
        values.recipient.clone(),
        ConversationKind::Direct,
        values.message_key.to_storage().to_string(),
        1,
    );
    let stored = InboxRepository::upsert_with_groupchat_notification_recovery(
        transaction,
        values.principal.bare_jid(),
        entry,
        true,
        groupchat_notification_recovery(values),
    )
    .await
    .expect("upsert inbox entry and recovery in UoW");
    assert_eq!(stored.partner, values.recipient);
    assert_eq!(stored.unread, 1);
}

#[cfg(feature = "clustering")]
fn archived_message(values: &FixtureValues) -> ArchivedMessage {
    ArchivedMessage {
        id: values.mam_id.clone(),
        timestamp: Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .expect("fixed fixture timestamp"),
        from: values.sender.clone().into(),
        to: values.recipient.clone().into(),
        body: Some("proof message".to_string()),
        stanza_id: None,
        thread: None,
        reply: None,
        origin_id: Some(values.origin_id.clone()),
        message_type: MessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    }
}

#[cfg(feature = "clustering")]
fn groupchat_notification_recovery(values: &FixtureValues) -> GroupchatNotificationRecovery {
    GroupchatNotificationRecovery {
        key: GroupchatNotificationRecoveryKey {
            recipient: values.principal.bare_jid().clone(),
            room: values.archive_jid.clone(),
            thread_id: None,
            archive_stanza_id: StanzaId::new(
                values.mam_id.clone(),
                jid::Jid::from(values.archive_jid.clone()),
            ),
        },
        sender_jid: jid::Jid::from(values.sender.clone()),
        is_live_occupant: true,
        room_members_only: false,
        sender_can_broadcast_channel_mention: false,
        created_at_ms: 1,
    }
}

#[cfg(feature = "clustering")]
struct FixtureValues {
    stream_id: SmSessionId,
    owner: NodeIdentity,
    claim_epoch: ClaimEpoch,
    principal: AuthenticatedPrincipalRef,
    sender: jid::BareJid,
    recipient: jid::BareJid,
    archive_jid: jid::BareJid,
    target: NormalizedTarget,
    origin_id: OriginId,
    digest: SemanticDigest,
    message_key: MessageKey,
    sm_ingress_id: SmIngressId,
    ordinal: IngressOrdinal,
    delivery_key: DeliveryKey,
    mam_id: String,
}

#[cfg(feature = "clustering")]
impl FixtureValues {
    fn new(name: &str) -> Self {
        let bare_jid: jid::BareJid = "romeo@example.com".parse().expect("valid fixture bare JID");
        let recipient: jid::BareJid = "juliet@example.com"
            .parse()
            .expect("valid fixture recipient JID");
        let archive_jid: jid::BareJid = format!("room-{name}@conference.example.com")
            .parse()
            .expect("valid fixture archive JID");
        Self {
            stream_id: SmSessionId::new(format!("stream-{name}")),
            owner: NodeIdentity::new("node-a", "node-a-epoch"),
            claim_epoch: ClaimEpoch(11),
            principal: AuthenticatedPrincipalRef::new(
                bare_jid.clone(),
                AuthContextId::new(
                    Uuid::parse_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90011")
                        .expect("valid auth context UUID"),
                ),
                AuthContextVersion::new(7),
                PrincipalAuthEpoch::new(9),
            ),
            sender: bare_jid,
            target: NormalizedTarget::Bare(recipient.clone()),
            recipient,
            archive_jid,
            origin_id: OriginId::new(format!("origin-{name}")),
            digest: digest(42),
            message_key: MessageKey::new(),
            sm_ingress_id: SmIngressId::new(),
            ordinal: IngressOrdinal::FIRST,
            delivery_key: DeliveryKey::new(),
            mam_id: format!("mam-{name}"),
        }
    }
}

struct Fixture {
    db: Database,
    uow: PostgresIngressUnitOfWork,
    #[cfg(feature = "clustering")]
    lineage: LineageConfig,
    /// The canonical identity source bound into `uow` (clustering only).
    #[cfg(feature = "clustering")]
    node_identity: SharedNodeIdentity,
    admin: sqlx::PgPool,
    schema: String,
    #[cfg(feature = "clustering")]
    schema_url: String,
}

impl Fixture {
    async fn open(test_name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress uow)");
            return None;
        };
        let schema = format!(
            "waddle_test_ingress_uow_{test_name}_{}",
            Uuid::new_v4().simple()
        );
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect postgres admin pool");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated postgres schema");
        let schema_url = postgres_url_with_search_path(&database_url, &schema);
        let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
        config.pool_size = 10;
        let db = Database::from_config("ingress-uow-test", &config)
            .await
            .expect("open isolated postgres database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply migrations to isolated schema");
        initialize_existing_store_schemas(&db, &schema_url).await;
        let lineage = fixture_lineage_config();
        lineage::enroll(&db, &lineage)
            .await
            .expect("enroll fixture lineage before UoW use");
        #[cfg(feature = "clustering")]
        let node_identity = SharedNodeIdentity::new(NodeIdentity::new("node-a", "node-a-epoch"));
        #[cfg(feature = "clustering")]
        let uow = PostgresIngressUnitOfWork::open_with_node_identity(
            db.clone(),
            lineage.clone(),
            node_identity.clone(),
        )
        .expect("open fixture UoW");
        #[cfg(not(feature = "clustering"))]
        let uow =
            PostgresIngressUnitOfWork::open(db.clone(), lineage.clone()).expect("open fixture UoW");
        Some(Self {
            db,
            uow,
            #[cfg(feature = "clustering")]
            lineage,
            #[cfg(feature = "clustering")]
            node_identity,
            admin,
            schema,
            #[cfg(feature = "clustering")]
            schema_url,
        })
    }

    fn uow(&self) -> PostgresIngressUnitOfWork {
        self.uow.clone()
    }

    async fn begin(&self) -> super::IngressUowTransaction<'_> {
        self.uow.begin().await.expect("begin fixture UoW")
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, crate::db::DatabaseError> {
        let conn = self.db.guard().await?;
        conn.execute(sql, params).await
    }

    #[cfg(feature = "clustering")]
    async fn seed_claim(&self, values: &FixtureValues) {
        self.execute(
                "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    sm_claim_entity(&values.stream_id),
                    EntityType::SmSession.as_db_str().to_string(),
                    values.owner.node_id.clone(),
                    values.owner.node_epoch.clone(),
                    values.claim_epoch.0,
                ],
            )
            .await
            .expect("seed exact SM claim");
    }

    #[cfg(feature = "clustering")]
    async fn seed_room_claim(&self, values: &FixtureValues) {
        self.execute(
            "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                room_claim_entity(&values.archive_jid),
                EntityType::RoomActor.as_db_str().to_string(),
                values.owner.node_id.clone(),
                values.owner.node_epoch.clone(),
                values.claim_epoch.0,
            ],
        )
        .await
        .expect("seed exact room claim");
    }

    #[cfg(feature = "clustering")]
    async fn seed_claim_and_session(&self, values: &FixtureValues, inbound_count: u32) {
        self.seed_claim(values).await;
        self.execute(
            SESSION_INSERT_SQL,
            session_insert_params(values, inbound_count),
        )
        .await
        .expect("seed SM session with typed principal identity");
    }

    async fn set_epoch(&self, epoch: u32) {
        self.execute(
                "UPDATE ingress_protocol_epoch SET epoch = ?, activated_at = now(), lineage_uuid = ?::uuid WHERE id = 1",
                crate::db_params![
                    i64::from(epoch),
                    "8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92".to_string(),
                ],
            )
            .await
            .expect("set live protocol epoch");
    }

    async fn advance_epoch_to_one(&self) {
        self.set_epoch(1).await;
    }

    #[cfg(feature = "clustering")]
    async fn assert_spanning_rows(&self, values: &FixtureValues, expected_count: i64) {
        for table in [
            "ingress_messages",
            "ingress_origin_aliases",
            "ingress_sm_refs",
            "ingress_deliveries",
            "mam_messages",
            "inbox_entries",
            "groupchat_notification_recovery",
        ] {
            assert_eq!(
                self.count(table).await,
                expected_count,
                "{table} visibility"
            );
        }
        if expected_count == 1 {
            let conn = self.db.guard().await.expect("fresh database connection");
            let mut rows = conn
                .query(
                    "SELECT inbound_count, bare_jid, auth_context_id, auth_context_version, principal_auth_epoch FROM sm_sessions WHERE stream_id = ?",
                    crate::db_params![values.stream_id.as_str().to_string()],
                )
                .await
                .expect("read committed SM principal row");
            let row = rows
                .next()
                .await
                .expect("read SM principal row")
                .expect("SM row exists");
            assert_eq!(row.get::<i64>(0).expect("decode frontier"), 1);
            assert_eq!(
                row.get::<String>(1).expect("decode bare JID"),
                values.principal.bare_jid().to_string()
            );
            assert_eq!(
                row.get::<String>(2).expect("decode context ID"),
                values.principal.auth_context_id().as_uuid().to_string()
            );
            assert_eq!(row.get::<i64>(3).expect("decode context version"), 7);
            assert_eq!(row.get::<i64>(4).expect("decode auth epoch"), 9);
            assert!(
                self.row_exists(
                    "SELECT 1 FROM ingress_origin_aliases WHERE message_key = ?::uuid",
                    crate::db_params![values.message_key.to_storage().to_string()],
                )
                .await,
                "origin alias retains the canonical message key"
            );
            assert!(
                self.row_exists(
                    "SELECT 1 FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric AND message_key = ?::uuid",
                    crate::db_params![
                        values.sm_ingress_id.to_storage().to_string(),
                        values.ordinal.to_storage().to_string(),
                        values.message_key.to_storage().to_string(),
                    ],
                )
                .await,
                "SM ingress reference retains its exact typed identity"
            );
            assert!(
                self.row_exists(
                    "SELECT 1 FROM ingress_deliveries WHERE delivery_key = ?::uuid AND message_key = ?::uuid",
                    crate::db_params![
                        values.delivery_key.to_storage().to_string(),
                        values.message_key.to_storage().to_string(),
                    ],
                )
                .await,
                "delivery identity retains the canonical message key"
            );
            assert!(
                self.row_exists(
                    "SELECT 1 FROM mam_messages WHERE id = ?",
                    crate::db_params![values.mam_id.clone()],
                )
                .await,
                "MAM identity is committed"
            );
        }
    }

    #[cfg(feature = "clustering")]
    async fn row_exists(&self, sql: &str, params: impl IntoParams) -> bool {
        let conn = self.db.guard().await.expect("fresh database connection");
        let mut rows = conn.query(sql, params).await.expect("query committed row");
        rows.next().await.expect("read committed row").is_some()
    }

    #[cfg(feature = "clustering")]
    async fn count(&self, table: &str) -> i64 {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .expect("count rows");
        rows.next()
            .await
            .expect("read count row")
            .expect("count row exists")
            .get(0)
            .expect("decode count")
    }

    #[cfg(feature = "clustering")]
    async fn assert_session_absent(&self, stream_id: &SmSessionId) {
        assert!(
            !self
                .row_exists(
                    "SELECT 1 FROM sm_sessions WHERE stream_id = ?",
                    crate::db_params![stream_id.as_str().to_string()],
                )
                .await,
            "principal-bearing session row must roll back with the UoW"
        );
    }

    #[cfg(feature = "clustering")]
    async fn delete_session(&self, stream_id: &SmSessionId) {
        self.execute(
            "DELETE FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .expect("delete fixture session");
    }

    async fn close(self) {
        let Self {
            db,
            uow,
            admin,
            schema,
            ..
        } = self;
        drop(uow);
        drop(db);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop isolated postgres schema");
    }
}

async fn initialize_existing_store_schemas(db: &Database, schema_url: &str) {
    #[cfg(not(feature = "clustering"))]
    let _ = db;
    #[cfg(feature = "clustering")]
    {
        let claims = crate::clustering::claims::PostgresClaimStore::new(db.clone());
        claims
            .ensure_schema()
            .await
            .expect("initialize claims schema");
    }
    crate::sm_persistence::DatabaseSmPersistence::open(Some(schema_url))
        .await
        .expect("initialize SM persistence schema");
    crate::inbox::DatabaseInboxStorage::open(Some(schema_url))
        .await
        .expect("initialize inbox schema");
    waddle_xmpp::mam::SqlxMamStorage::open(schema_url)
        .await
        .expect("initialize MAM schema");
}

fn fixture_lineage_config() -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001"
                .parse()
                .expect("valid fixture deployment UUID"),
        ),
        action: None,
    }
}

#[cfg(feature = "clustering")]
async fn seed_principal_session(
    fixture: &Fixture,
    principal: &AuthenticatedPrincipalRef,
    expires_at: Option<chrono::DateTime<Utc>>,
) -> String {
    let user_jid = principal.bare_jid().to_string();
    let suffix = Uuid::new_v4().simple().to_string();
    fixture
        .execute(
            "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                user_jid.clone(),
                format!("principal-{suffix}"),
                format!("principal-{suffix}"),
                Utc::now().to_rfc3339(),
                Utc::now().to_rfc3339(),
            ],
        )
        .await
        .expect("seed principal user");
    let session_id = format!("principal-session-{suffix}");
    fixture
        .execute(
            "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, expires_at, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            crate::db_params![
                session_id.clone(),
                user_jid,
                format!("principal-token-{suffix}"),
                principal.auth_context_id().as_uuid().to_string(),
                i64::try_from(principal.auth_context_version().get()).expect("version fits i64"),
                i64::try_from(principal.auth_epoch().get()).expect("epoch fits i64"),
                expires_at.map(|value| value.to_rfc3339()),
                Utc::now().to_rfc3339(),
                Utc::now().to_rfc3339(),
            ],
        )
        .await
        .expect("seed principal session");
    session_id
}

#[cfg(feature = "clustering")]
const SESSION_INSERT_SQL: &str = r#"INSERT INTO sm_sessions (
        stream_id, user_id, full_jid, inbound_count, outbound_count, last_acked,
        detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested,
        blocklist_interested, presence_available, presence_priority, bare_jid,
        auth_context_id, auth_context_version, principal_auth_epoch
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#;

#[cfg(feature = "clustering")]
fn session_insert_params(values: &FixtureValues, inbound_count: u32) -> Vec<crate::db::Value> {
    crate::db_params![
        values.stream_id.as_str().to_string(),
        "romeo".to_string(),
        "romeo@example.com/phone".to_string(),
        i64::from(inbound_count),
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        values.principal.bare_jid().to_string(),
        values.principal.auth_context_id().as_uuid().to_string(),
        i64::try_from(values.principal.auth_context_version().get()).expect("version fits i64"),
        i64::try_from(values.principal.auth_epoch().get()).expect("epoch fits i64"),
    ]
}

/// Insert the principal-bearing session row INSIDE the unit of work, after
/// the exact claim fence, so the spanning proof actually spans exact
/// principal identity (sol implementation-review finding 1).
#[cfg(feature = "clustering")]
async fn insert_session_in_uow(
    transaction: &mut IngressUowTransaction<'_>,
    values: &FixtureValues,
    inbound_count: u32,
) {
    transaction
        .transaction_mut()
        .execute(
            SESSION_INSERT_SQL,
            session_insert_params(values, inbound_count),
        )
        .await
        .expect("insert principal-bearing SM session inside the UoW");
}

#[cfg(feature = "clustering")]
fn sm_claim_entity(stream_id: &SmSessionId) -> String {
    format!(
        "{}:{}",
        EntityType::SmSession.as_db_str(),
        stream_id.as_str()
    )
}

#[cfg(feature = "clustering")]
fn room_claim_entity(room: &jid::BareJid) -> String {
    format!("{}:{room}", EntityType::RoomActor.as_db_str())
}

fn digest(byte: u8) -> SemanticDigest {
    SemanticDigest::from_storage(1, [byte; 32]).expect("valid fixture semantic digest")
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

#[cfg(feature = "clustering")]
async fn wait_for_lock_waiter(admin: &sqlx::PgPool, fragment: &str) {
    // Time-based budget (~10s), matching the ingress_substrate helper: the
    // competing backend must connect and reach the lock before we assert.
    for _ in 0..400 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE $1",
        )
        .bind(format!("%{fragment}%"))
        .fetch_one(admin)
        .await
        .expect("poll pg_stat_activity for a lock waiter");
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("no blocked backend appeared for query fragment {fragment:?}");
}
