//! Literal replay bytes across SQLite/Postgres persistence and fresh registries.
//! XEP-0198 requires retransmission; #1658 additionally promises these bytes.

pub mod ingress_support;
mod replay_bytes_support;

use ingress_support::IngressFixture;
use replay_bytes_support::ReplayBytesFixture;
use std::sync::Arc;
use waddle_server::sm_persistence::DatabaseSmPersistence;
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};

async fn persisted_replay_bytes(fixture: IngressFixture) {
    let storage = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM storage"),
    );
    let payloads =
        ReplayBytesFixture::new("exact-replay", "alice@example.com/web".parse().unwrap());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    payloads.store_and_append(&registry).await;
    payloads.retry_appends(&registry).await;
    drop(registry);
    for _ in 0..2 {
        let restored = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
        assert_eq!(
            restored
                .restore_from_persistence()
                .await
                .expect("fresh registry restore"),
            1
        );
        payloads.retry_appends(&restored).await;
        let claimed = restored
            .claim_session("exact-replay")
            .await
            .expect("resume claim")
            .expect("persisted session");
        payloads.assert_replay(&claimed);
        payloads.complete_resume_and_detach(&restored).await;
        drop(restored);
    }
    drop(storage);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_replay_preserves_production_bytes_and_original_delay() {
    persisted_replay_bytes(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_replay_preserves_production_bytes_and_original_delay() {
    let Some(fixture) = IngressFixture::postgres("replay_bytes").await else {
        return;
    };
    persisted_replay_bytes(fixture).await;
}

#[tokio::test]
async fn sqlite_live_snapshot_and_detached_drains_preserve_captured_bytes() {
    let fixture = IngressFixture::sqlite().await;
    let storage = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .unwrap(),
    );
    let payloads =
        ReplayBytesFixture::new("literal-replay", "alice@example.com/web".parse().unwrap());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let expected = payloads.store_literal_snapshot_and_appends(&registry).await;
    drop(registry);
    let restored = InMemorySmSessionRegistry::new().with_persistence(storage);
    assert_eq!(restored.restore_from_persistence().await.unwrap(), 1);
    let session = restored
        .peek_session("literal-replay")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.stanzas_to_resend(0), expected);
    drop(restored);
    fixture.close().await;
}
