//! Durable projection of the user-published Waddle DND state.
//!
//! Canonical state lives in the user's PEP node `urn:waddle:dnd:0`;
//! this single-row-per-user table is the indexed view consulted by
//! the T1 push gate. Storage is the XML payload as text — parsed
//! exactly once at read time per the typed-payloads hard rule.
//!
//! ## Mutation flow
//!
//! `pubsub/item.rs` writes the projection in the same transaction as
//! the `pubsub_items` insert (publish), retract, and node purge — all
//! via [`upsert_dnd_projection_tx`] / [`delete_dnd_projection_tx`] so
//! there is exactly one place that knows the table layout.
//!
//! ## LWW arbitration
//!
//! Concurrent publishes from two resources race for the SQLite write
//! lock. `source_version` is stamped from `published_at_ms` at the
//! same point in the publish path; the `ON CONFLICT` clause includes
//! `WHERE source_version < excluded.source_version` so a slow-to-commit
//! older write cannot stomp a newer one. Same-ms collisions are
//! deterministically resolved by `published_at_ms` ties going to the
//! row already in the table — that's the SQLite UPSERT contract when
//! the guard `excluded.source_version > source_version` fails.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;
use waddle_xmpp::xep::xep_waddle_dnd::{DndParseError, WaddleDnd};

use crate::db::{Database, DatabaseError, Transaction};

/// One projection row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DndProjection {
    pub owner_bare_jid: BareJid,
    pub state: WaddleDnd,
    pub source_version: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Error)]
pub enum DndProjectionError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
    #[error("serialize DND payload XML: {0}")]
    Serialize(String),
    #[error("stored DND projection XML is not parseable: {0}")]
    StoredPayloadParse(String),
    #[error("invalid owner bare JID: {0}")]
    InvalidOwnerBareJid(String),
    #[error("invalid DND payload: {0}")]
    InvalidPayload(#[from] DndParseError),
}

#[derive(Clone)]
pub struct DndProjectionStore {
    db: Database,
}

impl DndProjectionStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn upsert(&self, projection: &DndProjection) -> Result<(), DndProjectionError> {
        let mut tx = self.db.begin().await?;
        upsert_dnd_projection_tx(&mut tx, projection).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete(&self, owner_bare_jid: &BareJid) -> Result<bool, DndProjectionError> {
        let mut tx = self.db.begin().await?;
        let affected = delete_dnd_projection_tx(&mut tx, owner_bare_jid).await?;
        tx.commit().await?;
        Ok(affected > 0)
    }

    pub async fn get(
        &self,
        owner_bare_jid: &BareJid,
    ) -> Result<Option<DndProjection>, DndProjectionError> {
        let conn = self.db.guard().await?;
        let mut rows = conn
            .query(
                r#"
                SELECT owner_bare_jid, payload_xml, source_version, updated_at_ms
                FROM dnd_projection
                WHERE owner_bare_jid = ?
                "#,
                crate::db_params![owner_bare_jid.to_string()],
            )
            .await?;
        rows.next().await?.map(decode_row).transpose()
    }
}

/// Upsert a DND projection row inside an open transaction.
///
/// The `ON CONFLICT … WHERE excluded.source_version > source_version`
/// guard makes the upsert idempotent under concurrent racers: an
/// older publish that lost the write-lock race CANNOT overwrite a
/// newer one that already committed. Equal source_version (same-ms
/// collisions on wall-clock millis) is also rejected — first writer
/// wins for ties.
pub async fn upsert_dnd_projection_tx(
    tx: &mut Transaction<'_>,
    projection: &DndProjection,
) -> Result<(), DndProjectionError> {
    let payload_xml = element_to_string(&projection.state.to_element())?;
    tx.execute(
        r#"
        INSERT INTO dnd_projection (
            owner_bare_jid,
            payload_xml,
            source_version,
            updated_at_ms
        )
        VALUES (?, ?, ?, ?)
        ON CONFLICT(owner_bare_jid) DO UPDATE SET
            payload_xml = excluded.payload_xml,
            source_version = excluded.source_version,
            updated_at_ms = excluded.updated_at_ms
        WHERE excluded.source_version > dnd_projection.source_version
        "#,
        crate::db_params![
            projection.owner_bare_jid.to_string(),
            payload_xml,
            projection.source_version,
            projection.updated_at_ms,
        ],
    )
    .await?;
    Ok(())
}

/// Delete the projection row inside an open transaction. Returns the
/// number of rows removed (0 or 1).
pub async fn delete_dnd_projection_tx(
    tx: &mut Transaction<'_>,
    owner_bare_jid: &BareJid,
) -> Result<u64, DndProjectionError> {
    let affected = tx
        .execute(
            "DELETE FROM dnd_projection WHERE owner_bare_jid = ?",
            crate::db_params![owner_bare_jid.to_string()],
        )
        .await?;
    Ok(affected)
}

fn decode_row(row: crate::db::Row) -> Result<DndProjection, DndProjectionError> {
    let owner_bare_jid_text: String = row.get(0)?;
    let payload_xml: String = row.get(1)?;
    let source_version: i64 = row.get(2)?;
    let updated_at_ms: i64 = row.get(3)?;
    let owner_bare_jid: BareJid = owner_bare_jid_text
        .parse()
        .map_err(|_| DndProjectionError::InvalidOwnerBareJid(owner_bare_jid_text.clone()))?;
    let element: Element = payload_xml
        .parse()
        .map_err(|err: minidom::Error| DndProjectionError::StoredPayloadParse(err.to_string()))?;
    let state = WaddleDnd::parse(&element)?;
    Ok(DndProjection {
        owner_bare_jid,
        state,
        source_version,
        updated_at_ms,
    })
}

fn element_to_string(element: &Element) -> Result<String, DndProjectionError> {
    let mut buf: Vec<u8> = Vec::new();
    element
        .write_to(&mut buf)
        .map_err(|error| DndProjectionError::Serialize(error.to_string()))?;
    String::from_utf8(buf).map_err(|error| DndProjectionError::Serialize(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveTime, TimeZone, Utc, Weekday};
    use chrono_tz::Tz;
    use jid::BareJid;
    use std::str::FromStr;
    use waddle_xmpp::xep::xep_waddle_dnd::{ScheduleRule, WeekdaySet};

    fn alice() -> BareJid {
        BareJid::from_str("alice@waddle.example").expect("test JID parses")
    }

    fn sample_state() -> WaddleDnd {
        WaddleDnd {
            timezone: Tz::Europe__Oslo,
            snooze: Some(Utc.with_ymd_and_hms(2026, 5, 23, 17, 0, 0).unwrap()),
            rules: vec![ScheduleRule {
                days: WeekdaySet::from_iter([Weekday::Mon, Weekday::Tue, Weekday::Wed]),
                start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            }],
        }
    }

    async fn migrated_in_memory_store() -> DndProjectionStore {
        let storage = crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage");
        DndProjectionStore::new(storage.database())
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips_state() {
        let store = migrated_in_memory_store().await;
        let projection = DndProjection {
            owner_bare_jid: alice(),
            state: sample_state(),
            source_version: 1,
            updated_at_ms: 1_700_000_000_000,
        };
        store.upsert(&projection).await.expect("upsert");
        let loaded = store
            .get(&alice())
            .await
            .expect("get")
            .expect("row present");
        assert_eq!(loaded, projection);
    }

    #[tokio::test]
    async fn upsert_strictly_increasing_source_version_overwrites() {
        let store = migrated_in_memory_store().await;
        let first = DndProjection {
            owner_bare_jid: alice(),
            state: sample_state(),
            source_version: 1,
            updated_at_ms: 1_700_000_000_000,
        };
        store.upsert(&first).await.expect("first upsert");
        let mut second_state = sample_state();
        second_state.snooze = None;
        let second = DndProjection {
            owner_bare_jid: alice(),
            state: second_state.clone(),
            source_version: 2,
            updated_at_ms: 1_700_000_001_000,
        };
        store.upsert(&second).await.expect("second upsert");
        let loaded = store
            .get(&alice())
            .await
            .expect("get")
            .expect("row present");
        assert_eq!(loaded.state, second_state);
        assert_eq!(loaded.source_version, 2);
    }

    /// A losing publisher in a same-ms race (or a wall-clock-jump
    /// regression) MUST NOT overwrite the winner. The guard
    /// `excluded.source_version > dnd_projection.source_version`
    /// drops equal-or-older writes silently.
    #[tokio::test]
    async fn upsert_equal_or_older_source_version_is_ignored() {
        let store = migrated_in_memory_store().await;
        let winner = DndProjection {
            owner_bare_jid: alice(),
            state: sample_state(),
            source_version: 100,
            updated_at_ms: 1_700_000_000_000,
        };
        store.upsert(&winner).await.expect("winner upsert");
        let stale_equal = DndProjection {
            owner_bare_jid: alice(),
            state: WaddleDnd::empty_utc(),
            source_version: 100,
            updated_at_ms: 1_700_000_000_500,
        };
        store.upsert(&stale_equal).await.expect("guarded upsert");
        let loaded = store
            .get(&alice())
            .await
            .expect("get")
            .expect("row present");
        assert_eq!(
            loaded, winner,
            "same-source_version upsert must NOT overwrite winner"
        );
        let stale_older = DndProjection {
            owner_bare_jid: alice(),
            state: WaddleDnd::empty_utc(),
            source_version: 50,
            updated_at_ms: 1_700_000_000_900,
        };
        store.upsert(&stale_older).await.expect("guarded upsert");
        let loaded = store
            .get(&alice())
            .await
            .expect("get")
            .expect("row present");
        assert_eq!(
            loaded, winner,
            "older-source_version upsert must NOT overwrite winner"
        );
    }

    #[tokio::test]
    async fn delete_removes_row_get_returns_none() {
        let store = migrated_in_memory_store().await;
        let projection = DndProjection {
            owner_bare_jid: alice(),
            state: WaddleDnd::empty_utc(),
            source_version: 1,
            updated_at_ms: 1_700_000_000_000,
        };
        store.upsert(&projection).await.expect("upsert");
        let removed = store.delete(&alice()).await.expect("delete");
        assert!(removed);
        let loaded = store.get(&alice()).await.expect("get");
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn get_missing_row_returns_none() {
        let store = migrated_in_memory_store().await;
        let loaded = store.get(&alice()).await.expect("get");
        assert!(loaded.is_none());
    }

    /// `payload.ns()` MUST match the DND namespace. Defense-in-depth
    /// for the publish hook — even if dispatch routes a misnamed
    /// payload here, the typed parse will reject it via
    /// `WaddleDnd::parse` (`DndParseError::WrongRoot`). This test
    /// exercises the wrong-namespace path directly via `parse`.
    #[test]
    fn parse_wrong_namespace_yields_wrong_root_error() {
        let bad: Element = "<dnd xmlns='urn:example:other'/>".parse().unwrap();
        let result = WaddleDnd::parse(&bad);
        assert!(matches!(result, Err(DndParseError::WrongRoot { .. })));
        // Validate that `?` from `WaddleDnd::parse` lifts cleanly into
        // a `DndProjectionError::InvalidPayload`.
        fn lift(element: &Element) -> Result<WaddleDnd, DndProjectionError> {
            Ok(WaddleDnd::parse(element)?)
        }
        assert!(matches!(
            lift(&bad),
            Err(DndProjectionError::InvalidPayload(_))
        ));
    }

    #[test]
    fn parse_invalid_payload_yields_typed_error() {
        let bad: Element = "<dnd xmlns='urn:waddle:dnd:0' timezone='not_a_zone'/>"
            .parse()
            .unwrap();
        let result = WaddleDnd::parse(&bad);
        assert!(matches!(result, Err(DndParseError::InvalidTimezone(_))));
    }

    /// The `delete_dnd_projection_tx` helper is invoked from
    /// `pubsub/item.rs::retract_item_impl` and `purge_node_impl` to
    /// clear the projection when the user retracts or purges their
    /// own `urn:waddle:dnd:0` node. Skipping that cleanup would leave
    /// a stale projection row and silently suppress push notifications
    /// even after the user explicitly cleared their DND state.
    #[tokio::test]
    async fn delete_tx_removes_row_for_owner() {
        let storage = crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage");
        let db = storage.database();
        let store = DndProjectionStore::new(db.clone());
        store
            .upsert(&DndProjection {
                owner_bare_jid: alice(),
                state: sample_state(),
                source_version: 1,
                updated_at_ms: 1_700_000_000_000,
            })
            .await
            .expect("upsert");
        {
            let mut tx = db.begin().await.expect("begin");
            let removed = delete_dnd_projection_tx(&mut tx, &alice())
                .await
                .expect("delete in tx");
            assert_eq!(removed, 1);
            tx.commit().await.expect("commit");
        }
        assert!(
            store.get(&alice()).await.expect("get").is_none(),
            "retract/purge MUST leave no stale projection row"
        );
    }

    /// Direct `upsert_dnd_projection_tx` test — the publish hook in
    /// `pubsub/item.rs` and the `DndProjectionStore::upsert` wrapper
    /// both share this implementation, so this test pins the contract.
    #[tokio::test]
    async fn upsert_tx_writes_row_visible_after_commit() {
        let storage = crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage");
        let db = storage.database();
        let projection = DndProjection {
            owner_bare_jid: alice(),
            state: sample_state(),
            source_version: 1,
            updated_at_ms: 1_700_000_000_000,
        };
        {
            let mut tx = db.begin().await.expect("begin");
            upsert_dnd_projection_tx(&mut tx, &projection)
                .await
                .expect("upsert in tx");
            tx.commit().await.expect("commit");
        }
        let store = DndProjectionStore::new(db);
        let loaded = store
            .get(&alice())
            .await
            .expect("get")
            .expect("row present after tx commit");
        assert_eq!(loaded, projection);
    }
}
