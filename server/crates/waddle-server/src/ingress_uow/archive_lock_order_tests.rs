//! PostgreSQL characterization of the archive/room-replay lock interaction.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use waddle_xmpp::ingress::{MessageKey, SemanticDigest};

use super::{CanonicalMessageRepository, ClaimRepository, IngressUowError};
use crate::db::{Database, Transaction};
use crate::ingress::test_support::IngressFixture;

const OBSERVATION_BOUND: Duration = Duration::from_secs(3);
const SCENARIO_BOUND: Duration = Duration::from_secs(22);

async fn backend_pid(tx: &mut Transaction<'_>) -> Result<i64> {
    let mut rows = tx
        .query("SELECT pg_catalog.pg_backend_pid()::bigint", ())
        .await?;
    Ok(rows.next().await?.context("backend PID row")?.get(0)?)
}

async fn wait_for_blocker(db: &Database, waiter: i64, blocker: i64) -> Result<()> {
    let observer = db.guard().await?;
    tokio::time::timeout(OBSERVATION_BOUND, async {
        loop {
            let mut rows = observer
                .query(
                    "SELECT ?::bigint::integer = ANY(pg_catalog.pg_blocking_pids(?::bigint::integer))",
                    crate::db_params![blocker, waiter],
                )
                .await?;
            let blocked: bool = rows.next().await?.context("blocking row")?.get(0)?;
            if blocked {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("expected PostgreSQL blocking edge was not observed")?
}

async fn wait_for_fixture_transactions(db: &Database, schema: &str) -> Result<()> {
    let observer = db.guard().await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut rows = observer
                .query(
                    "SELECT COUNT(*) FROM pg_catalog.pg_locks AS locks \
                     JOIN pg_catalog.pg_class AS relation ON relation.oid = locks.relation \
                     JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                     WHERE namespace.nspname = ?",
                    crate::db_params![schema.to_owned()],
                )
                .await?;
            let count: i64 = rows.next().await?.context("fixture locks row")?.get(0)?;
            if count == 0 {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("fixture transactions still hold PostgreSQL relation locks")?
}

/// Each task owns its transaction. Dropping the JoinSet aborts all tasks if
/// setup, observation, or the outer deadline fails, so fixture cleanup cannot
/// wait forever on a detached transaction.
async fn observe_queued_takeover(
    fixture: &IngressFixture,
    room: &jid::BareJid,
    fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    key: MessageKey,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    let (archive_started, archive_pid) = oneshot::channel();
    let (archive_proceed, request_claim) = oneshot::channel();
    let (archive_claimed, claim_acquired) = oneshot::channel();
    let (archive_release, release_archive) = oneshot::channel();
    let uow = fixture.uow.clone();
    let archive_room = room.clone();
    let archive_fence = fence.clone();
    tasks.spawn(async move {
        let mut tx = uow
            .begin_with_timeouts(Duration::from_secs(3), Duration::from_secs(8))
            .await?;
        if !CanonicalMessageRepository::lock(&mut tx, key).await? {
            bail!("canonical message is missing");
        }
        archive_started
            .send(backend_pid(tx.transaction_mut()).await?)
            .map_err(|_| anyhow!("archive observer left"))?;
        request_claim.await.context("archive continuation")?;
        let started = std::time::Instant::now();
        ClaimRepository::assert_room_claim(
            &mut tx,
            &archive_room,
            &archive_fence.owner,
            archive_fence.epoch,
        )
        .await?;
        eprintln!(
            "archive acquired matching room claim despite queued takeover after {:?}",
            started.elapsed()
        );
        archive_claimed
            .send(())
            .map_err(|_| anyhow!("archive claim observer left"))?;
        release_archive.await.context("archive release")?;
        tx.commit().await?;
        Ok::<_, anyhow::Error>(())
    });
    let archive_pid = archive_pid.await.context("archive lock acquired")?;

    let (room_started, room_pid) = oneshot::channel();
    let (room_proceed, request_child) = oneshot::channel();
    let (child_started, child_pid) = oneshot::channel();
    let uow = fixture.uow.clone();
    let replay_room = room.clone();
    let replay_fence = fence.clone();
    tasks.spawn(async move {
        let mut outer = uow
            .begin_with_timeouts(Duration::from_secs(12), Duration::from_secs(15))
            .await?;
        CanonicalMessageRepository::protect_room_acceptance(
            &mut outer,
            &replay_room,
            &replay_fence,
        )
        .await?;
        room_started
            .send(backend_pid(outer.transaction_mut()).await?)
            .map_err(|_| anyhow!("room observer left"))?;
        request_child.await.context("child continuation")?;
        // Like room replay, retain the outer claim while the independent child
        // transaction acquires canonical authority and commits.
        let mut child = uow
            .begin_with_timeouts(Duration::from_secs(12), Duration::from_secs(15))
            .await?;
        child_started
            .send(backend_pid(child.transaction_mut()).await?)
            .map_err(|_| anyhow!("child observer left"))?;
        if !CanonicalMessageRepository::lock(&mut child, key).await? {
            bail!("child canonical message is missing");
        }
        child.commit().await?;
        outer.commit().await?;
        Ok(())
    });
    let room_pid = room_pid.await.context("room claim acquired")?;

    let (takeover_started, takeover_pid) = oneshot::channel();
    let db = fixture.db.clone();
    let entity = format!("room_actor:{room}");
    tasks.spawn(async move {
        let mut tx = db.begin().await?;
        tx.execute("SET LOCAL lock_timeout = '12s'", ()).await?;
        tx.execute("SET LOCAL statement_timeout = '15s'", ())
            .await?;
        takeover_started
            .send(backend_pid(&mut tx).await?)
            .map_err(|_| anyhow!("takeover observer left"))?;
        let changed = tx
            .execute(
                "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = ?",
                crate::db_params![entity],
            )
            .await?;
        if changed != 1 {
            bail!("takeover did not update the exact room claim");
        }
        tx.commit().await?;
        Ok(())
    });
    let takeover_pid = takeover_pid.await.context("takeover started")?;
    wait_for_blocker(&fixture.db, takeover_pid, room_pid)
        .await
        .context("takeover waiting on replay's room claim")?;
    eprintln!("observed PostgreSQL blocker: takeover {takeover_pid} -> room {room_pid}");
    archive_proceed
        .send(())
        .map_err(|_| anyhow!("archive left before requesting the claim"))?;
    tokio::time::timeout(OBSERVATION_BOUND, claim_acquired)
        .await
        .context("archive claim acquisition exceeded its deadline")?
        .context("archive did not acquire its room claim")?;
    wait_for_blocker(&fixture.db, takeover_pid, room_pid)
        .await
        .context("takeover no longer waiting while archive acquired the claim")?;
    room_proceed
        .send(())
        .map_err(|_| anyhow!("room replay left before requesting canonical authority"))?;
    let child_pid = child_pid.await.context("child transaction started")?;
    wait_for_blocker(&fixture.db, child_pid, archive_pid)
        .await
        .context("nested child waiting on archive canonical lock")?;
    eprintln!(
        "observed PostgreSQL blockers: takeover {takeover_pid} -> room {room_pid}; \
         child {child_pid} -> archive {archive_pid}; archive already acquired its claim"
    );
    archive_release
        .send(())
        .map_err(|_| anyhow!("archive left before release"))?;

    while let Some(result) = tasks.join_next().await {
        result.context("transaction task failed")??;
    }
    let mut stale = fixture.uow.begin().await?;
    let result =
        ClaimRepository::assert_room_claim(&mut stale, room, &fence.owner, fence.epoch).await;
    if !matches!(result, Err(IngressUowError::ClaimFenceMissing)) {
        bail!("old room claim remained usable after takeover: {result:?}");
    }
    drop(result);
    stale.commit().await?;
    eprintln!("old exact room claim was rejected after takeover committed");
    Ok(())
}

async fn observe_takeover_first(
    fixture: &IngressFixture,
    room: &jid::BareJid,
    fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    key: MessageKey,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    let (takeover_locked, takeover_pid) = oneshot::channel();
    let (takeover_release, release_takeover) = oneshot::channel();
    let db = fixture.db.clone();
    let entity = format!("room_actor:{room}");
    tasks.spawn(async move {
        let mut tx = db.begin().await?;
        tx.execute("SET LOCAL lock_timeout = '8s'", ()).await?;
        tx.execute("SET LOCAL statement_timeout = '12s'", ())
            .await?;
        let changed = tx
            .execute(
                "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = ?",
                crate::db_params![entity],
            )
            .await?;
        if changed != 1 {
            bail!("takeover did not lock the exact room claim");
        }
        takeover_locked
            .send(backend_pid(&mut tx).await?)
            .map_err(|_| anyhow!("takeover observer left"))?;
        release_takeover.await.context("takeover release")?;
        tx.rollback().await?;
        Ok::<_, anyhow::Error>(())
    });
    let takeover_pid = takeover_pid.await.context("takeover claim lock")?;

    let (archive_started, archive_pid) = oneshot::channel();
    let (archive_proceed, request_claim) = oneshot::channel();
    let (archive_timed_out, timeout_observed) = oneshot::channel();
    let uow = fixture.uow.clone();
    let archive_room = room.clone();
    let archive_fence = fence.clone();
    tasks.spawn(async move {
        // Production uses 100 ms. A longer bound makes the blocking edge
        // observable before the actual PostgreSQL lock timeout fires.
        let mut tx = uow
            .begin_with_timeouts(Duration::from_secs(4), Duration::from_secs(8))
            .await?;
        if !CanonicalMessageRepository::lock(&mut tx, key).await? {
            bail!("canonical message is missing");
        }
        archive_started
            .send(backend_pid(tx.transaction_mut()).await?)
            .map_err(|_| anyhow!("archive observer left"))?;
        request_claim.await.context("archive continuation")?;
        let started = std::time::Instant::now();
        let result = ClaimRepository::assert_room_claim(
            &mut tx,
            &archive_room,
            &archive_fence.owner,
            archive_fence.epoch,
        )
        .await;
        if !matches!(result, Err(IngressUowError::Timeout)) {
            bail!("archive claim did not reach the configured lock timeout: {result:?}");
        }
        eprintln!(
            "takeover-first archive claim returned Timeout after {:?}",
            started.elapsed()
        );
        drop(result);
        drop(tx);
        archive_timed_out
            .send(())
            .map_err(|_| anyhow!("archive timeout observer left"))?;
        Ok(())
    });
    let archive_pid = archive_pid.await.context("archive canonical lock")?;
    archive_proceed
        .send(())
        .map_err(|_| anyhow!("archive left before requesting the claim"))?;
    wait_for_blocker(&fixture.db, archive_pid, takeover_pid)
        .await
        .context("archive waiting on takeover-held claim")?;
    eprintln!("observed PostgreSQL blocker: archive {archive_pid} -> takeover {takeover_pid}");
    timeout_observed.await.context("archive lock timeout")?;
    takeover_release
        .send(())
        .map_err(|_| anyhow!("takeover left before release"))?;
    while let Some(result) = tasks.join_next().await {
        result.context("transaction task failed")??;
    }

    let mut probe = fixture.uow.begin().await?;
    ClaimRepository::assert_room_claim(&mut probe, room, &fence.owner, fence.epoch).await?;
    probe.commit().await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Case {
    QueuedTakeover,
    TakeoverFirst,
}

async fn scenario(
    fixture: &IngressFixture,
    room: &jid::BareJid,
    fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    case: Case,
) -> Result<()> {
    let key = MessageKey::new();
    let digest = SemanticDigest::from_storage(1, [7; 32])?;
    let mut seed = fixture.uow.begin().await?;
    CanonicalMessageRepository::record_message(&mut seed, key, &digest, None).await?;
    seed.commit().await?;

    match case {
        Case::QueuedTakeover => {
            // With no queued takeover, the two SHARE locks are compatible even
            // though the archive holds canonical authority first.
            let mut canonical = fixture
                .uow
                .begin_with_timeouts(Duration::from_secs(1), Duration::from_secs(2))
                .await?;
            if !CanonicalMessageRepository::lock(&mut canonical, key).await? {
                bail!("baseline canonical message is missing");
            }
            let mut replay = fixture
                .uow
                .begin_with_timeouts(Duration::from_secs(1), Duration::from_secs(2))
                .await?;
            CanonicalMessageRepository::protect_room_acceptance(&mut replay, room, fence).await?;
            ClaimRepository::assert_room_claim(&mut canonical, room, &fence.owner, fence.epoch)
                .await?;
            canonical.commit().await?;
            replay.commit().await?;
            observe_queued_takeover(fixture, room, fence, key).await
        }
        Case::TakeoverFirst => observe_takeover_first(fixture, room, fence, key).await,
    }
}

async fn run_case(case: Case) {
    let Some(mut fixture) =
        tokio::time::timeout(Duration::from_secs(45), IngressFixture::postgres("ar_lock"))
            .await
            .expect("fixture setup must remain bounded")
    else {
        return;
    };
    let room: jid::BareJid = "archive-order@muc.example.test".parse().expect("room");
    let schema = tokio::time::timeout(Duration::from_secs(5), async {
        let observer = fixture.db.guard().await.expect("schema observer");
        let mut rows = observer
            .query("SELECT pg_catalog.current_schema()::text", ())
            .await
            .expect("current schema");
        rows.next()
            .await
            .expect("schema row read")
            .expect("schema row")
            .get::<String>(0)
            .expect("schema name")
    })
    .await
    .expect("schema observation must remain bounded");

    let observed = tokio::time::timeout(SCENARIO_BOUND, async {
        let fence = fixture.room_fence(&room).await;
        scenario(&fixture, &room, &fence, case).await
    })
    .await;
    if let Err(error) = wait_for_fixture_transactions(&fixture.db, &schema).await {
        // Retain the isolated schema rather than issuing blocking DDL while a
        // server-side transaction may still hold a relation lock.
        drop(fixture);
        panic!("fixture cleanup refused: {error:#}");
    }
    tokio::time::timeout(Duration::from_secs(5), fixture.close())
        .await
        .expect("fixture cleanup must remain bounded");
    observed
        .expect("lock-order scenario exceeded its deadline")
        .expect("observed lock-order scenario");
}

#[tokio::test]
async fn postgres_archive_replay_with_queued_takeover() {
    run_case(Case::QueuedTakeover).await;
}

#[tokio::test]
async fn postgres_archive_claim_times_out_when_takeover_holds_claim() {
    run_case(Case::TakeoverFirst).await;
}
