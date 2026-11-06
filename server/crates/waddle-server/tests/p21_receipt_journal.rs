//! The SQLite receipt journal uses the caller's real shared backend.

use std::sync::Arc;

use uuid::Uuid;
use waddle_server::db::physical_attestation::{
    ReceiptAuthorityError, ReceiptCommit, ReceiptOperation, ReceiptOperationAuthority,
    ReceiptReadback,
};
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::ingress_substrate::EffectReceiptKind;
use waddle_xmpp::ingress::MessageKey;

async fn seed(db: &Database) -> ReceiptOperation {
    let operation = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        message: MessageKey::new(),
        kind: EffectReceiptKind::from_storage(1),
        semantic_identity_hash: [0x5a; 32],
    };
    let conn = db.guard().await.expect("seed connection");
    conn.execute(
        "INSERT INTO ingress_messages(message_key,digest_version,digest) VALUES (?,1,?)",
        waddle_server::db_params![operation.message.to_storage().to_string(), vec![0x4au8; 32]],
    )
    .await
    .expect("canonical message");
    conn.execute(
        "INSERT INTO ingress_effect_intents(message_key,effect_ordinal,kind,semantic_identity_hash,payload_version,payload) VALUES (?,'0',?,?,1,?)",
        waddle_server::db_params![operation.message.to_storage().to_string(),operation.kind.to_storage(), operation.semantic_identity_hash.to_vec(), vec![1u8]],
    )
    .await
    .expect("effect intent");
    operation
}

#[tokio::test]
async fn exact_operation_replay_conflict_and_historical_readback() {
    let db = Database::in_memory("receipt journal")
        .await
        .expect("database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    let operation = seed(&db).await;
    let authority = ReceiptOperationAuthority::sqlite(db.clone()).expect("shared backend");
    assert_eq!(
        authority.readback(&operation).await.expect("readback"),
        ReceiptReadback::Indeterminate
    );
    assert_eq!(
        authority.record(&operation).await.expect("record"),
        ReceiptCommit::Committed {
            operation_id: operation.operation_id,
            inserted: true
        }
    );
    assert_eq!(
        authority.record(&operation).await.expect("replay"),
        ReceiptCommit::Committed {
            operation_id: operation.operation_id,
            inserted: true
        }
    );
    assert_eq!(
        authority.readback(&operation).await.expect("readback"),
        ReceiptReadback::Committed { inserted: true }
    );

    let changed = ReceiptOperation {
        semantic_identity_hash: [0x6a; 32],
        ..operation
    };
    assert!(matches!(
        authority.record(&changed).await,
        Err(ReceiptAuthorityError::OperationConflict)
    ));
    assert!(matches!(
        authority.readback(&changed).await,
        Err(ReceiptAuthorityError::OperationConflict)
    ));
    let fresh_operation = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        ..operation
    };
    assert_eq!(
        authority
            .record(&fresh_operation)
            .await
            .expect("new operation on existing receipt"),
        ReceiptCommit::Committed {
            operation_id: fresh_operation.operation_id,
            inserted: false
        }
    );

    let conn = db.guard().await.expect("database connection");
    conn.execute(
        "DELETE FROM ingress_messages WHERE message_key=?",
        waddle_server::db_params![operation.message.to_storage().to_string()],
    )
    .await
    .expect("collect canonical message");
    drop(conn);
    assert_eq!(
        authority
            .readback(&operation)
            .await
            .expect("historical readback"),
        ReceiptReadback::Committed { inserted: true }
    );
    assert_eq!(
        authority
            .record(&operation)
            .await
            .expect("historical replay"),
        ReceiptCommit::Committed {
            operation_id: operation.operation_id,
            inserted: true
        }
    );

    let missing = ReceiptOperation {
        operation_id: Uuid::new_v4(),
        ..operation
    };
    assert!(
        authority.record(&missing).await.is_err(),
        "a vanished intent cannot mint a new journal result"
    );
    assert_eq!(
        authority.readback(&missing).await.expect("missing journal"),
        ReceiptReadback::Indeterminate
    );
    let nil = ReceiptOperation {
        operation_id: Uuid::nil(),
        ..operation
    };
    assert!(matches!(
        authority.record(&nil).await,
        Err(ReceiptAuthorityError::InvalidOperation)
    ));

    let conn = db.guard().await.expect("epoch connection");
    conn.execute(
        "UPDATE ingress_protocol_epoch SET epoch=4294967295, activated_at='test', lineage_uuid='test' WHERE id=1",
        (),
    )
    .await
    .expect("install unsupported epoch");
    drop(conn);
    assert!(
        authority.readback(&operation).await.is_err(),
        "an unsupported epoch fences even historical readback"
    );
}

#[tokio::test]
async fn simultaneous_calls_return_one_stable_result() {
    let db = Database::in_memory("receipt journal concurrency")
        .await
        .expect("database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    let operation = seed(&db).await;
    let authority = Arc::new(ReceiptOperationAuthority::sqlite(db).expect("shared backend"));
    let first = Arc::clone(&authority);
    let second = Arc::clone(&authority);
    let (first, second) = tokio::join!(first.record(&operation), second.record(&operation));
    let expected = ReceiptCommit::Committed {
        operation_id: operation.operation_id,
        inserted: true,
    };
    assert_eq!(first.expect("first"), expected);
    assert_eq!(second.expect("second"), expected);
}
