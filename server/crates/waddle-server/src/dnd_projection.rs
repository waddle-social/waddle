//! Durable projection of the user-published Waddle DND state.
//!
//! The canonical DND state lives in the user's PEP node
//! `urn:waddle:dnd:0`. This module stores a denormalized server-side
//! snapshot keyed by `owner_bare_jid` so the T1 push-gate can read
//! the recipient's DND state without round-tripping through the
//! PubSub item store.
//!
//! ## Source of truth
//!
//! The `pubsub_items` row published by the user is authoritative —
//! this projection is a derived view. A T1 read that finds no
//! projection row treats the user as not-in-DND
//! ([`crate::notification_outbox::DndState::Inactive`]).
//!
//! ## Mutation flow
//!
//! `pubsub_dispatch` calls [`derive_dnd_projection_mutation`] after a
//! successful publish to `urn:waddle:dnd:0` on the user's own PEP
//! service. The mutation enum carries a typed [`WaddleDnd`] for the
//! upsert path and a bare JID for the delete path (empty-payload
//! republish / retract).
//!
//! ## Storage shape
//!
//! Per the typed-payloads hard rule, the boundary into the typed
//! [`WaddleDnd`] happens exactly once on the read side. The stored
//! XML is the same `<dnd>` element the user published, so the
//! projection can never drift from the canonical PEP item. The
//! source_version column is a monotonic counter for LWW arbitration
//! across the (PEP publish, projection upsert) interleaving window.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;
use waddle_xmpp::xep::xep_waddle_dnd::{DndParseError, WaddleDnd, NS_WADDLE_DND_V0};

use crate::db::{Database, DatabaseError};

/// One projection row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DndProjection {
    pub owner_bare_jid: BareJid,
    pub state: WaddleDnd,
    pub source_version: i64,
    pub updated_at_ms: i64,
}

/// Mutation derived from a publish item. Distinct from `Option<…>`
/// so the call site at the publish boundary makes the
/// "explicit-delete vs leave-alone" decision concretely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DndProjectionMutation {
    Upsert(DndProjection),
    Delete { owner_bare_jid: BareJid },
}

#[derive(Debug, Error)]
pub enum DndProjectionError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
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
        let conn = self.db.guard().await?;
        let payload_xml = element_to_string(&projection.state.to_element());
        conn.execute(
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

    pub async fn delete(&self, owner_bare_jid: &BareJid) -> Result<bool, DndProjectionError> {
        let conn = self.db.guard().await?;
        let affected = conn
            .execute(
                "DELETE FROM dnd_projection WHERE owner_bare_jid = ?",
                crate::db_params![owner_bare_jid.to_string()],
            )
            .await?;
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

    /// Apply a derived mutation. Convenience wrapper used by the
    /// pubsub publish hook — it forwards to upsert or delete without
    /// the caller needing to pattern-match.
    pub async fn apply(&self, mutation: &DndProjectionMutation) -> Result<(), DndProjectionError> {
        match mutation {
            DndProjectionMutation::Upsert(projection) => self.upsert(projection).await,
            DndProjectionMutation::Delete { owner_bare_jid } => {
                let _ = self.delete(owner_bare_jid).await?;
                Ok(())
            }
        }
    }
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

fn element_to_string(element: &Element) -> String {
    let mut buf: Vec<u8> = Vec::new();
    element
        .write_to(&mut buf)
        .expect("writing a typed Element to an in-memory buffer cannot fail");
    String::from_utf8(buf).expect("Element::write_to always emits valid UTF-8")
}

/// Build a mutation from a PEP publish item. The publish hook calls
/// this on every successful `urn:waddle:dnd:0` publish on the user's
/// own PEP service.
///
/// * `payload` — the inner `<dnd>` element from the `<item><payload>`
///   wrapper, OR `None` when the user published an empty item (XEP-0060
///   retract-style "clear DND"). The empty case maps to
///   [`DndProjectionMutation::Delete`].
/// * `updated_at_ms` — wall-clock UTC millis at publish time.
/// * `source_version` — monotonic version for LWW; the caller bumps
///   on each publish.
pub fn derive_dnd_projection_mutation(
    owner_bare_jid: &BareJid,
    payload: Option<&Element>,
    source_version: i64,
    updated_at_ms: i64,
) -> Result<DndProjectionMutation, DndProjectionError> {
    let Some(payload) = payload else {
        return Ok(DndProjectionMutation::Delete {
            owner_bare_jid: owner_bare_jid.clone(),
        });
    };
    // Reject publish payloads from other namespaces. The pubsub
    // dispatch shouldn't route those here, but defense in depth.
    if payload.ns() != NS_WADDLE_DND_V0 {
        return Err(DndProjectionError::InvalidPayload(
            DndParseError::WrongRoot {
                name: payload.name().to_string(),
                ns: payload.ns(),
            },
        ));
    }
    let state = WaddleDnd::parse(payload)?;
    Ok(DndProjectionMutation::Upsert(DndProjection {
        owner_bare_jid: owner_bare_jid.clone(),
        state,
        source_version,
        updated_at_ms,
    }))
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
                days: WeekdaySet::from_iter(
                    [Weekday::Mon, Weekday::Tue, Weekday::Wed].iter().copied(),
                ),
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
    async fn upsert_overwrites_prior_state_via_lww() {
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

    #[test]
    fn derive_mutation_from_typed_payload_yields_upsert() {
        let state = sample_state();
        let element = state.to_element();
        let mutation =
            derive_dnd_projection_mutation(&alice(), Some(&element), 1, 1_700_000_000_000)
                .expect("derive");
        assert_eq!(
            mutation,
            DndProjectionMutation::Upsert(DndProjection {
                owner_bare_jid: alice(),
                state,
                source_version: 1,
                updated_at_ms: 1_700_000_000_000,
            })
        );
    }

    #[test]
    fn derive_mutation_empty_payload_yields_delete() {
        let mutation =
            derive_dnd_projection_mutation(&alice(), None, 1, 1_700_000_000_000).expect("derive");
        assert_eq!(
            mutation,
            DndProjectionMutation::Delete {
                owner_bare_jid: alice()
            }
        );
    }

    #[test]
    fn derive_mutation_wrong_namespace_rejected() {
        let bad: Element = "<dnd xmlns='urn:example:other'/>".parse().unwrap();
        let result = derive_dnd_projection_mutation(&alice(), Some(&bad), 1, 1_700_000_000_000);
        assert!(matches!(result, Err(DndProjectionError::InvalidPayload(_))));
    }

    #[test]
    fn derive_mutation_invalid_dnd_payload_rejected() {
        let bad: Element = "<dnd xmlns='urn:waddle:dnd:0' timezone='not_a_zone'/>"
            .parse()
            .unwrap();
        let result = derive_dnd_projection_mutation(&alice(), Some(&bad), 1, 1_700_000_000_000);
        assert!(matches!(
            result,
            Err(DndProjectionError::InvalidPayload(
                DndParseError::InvalidTimezone(_)
            ))
        ));
    }

    #[tokio::test]
    async fn apply_dispatches_upsert_and_delete_correctly() {
        let store = migrated_in_memory_store().await;
        let upsert = DndProjectionMutation::Upsert(DndProjection {
            owner_bare_jid: alice(),
            state: sample_state(),
            source_version: 1,
            updated_at_ms: 1_700_000_000_000,
        });
        store.apply(&upsert).await.expect("apply upsert");
        assert!(store.get(&alice()).await.unwrap().is_some());
        let delete = DndProjectionMutation::Delete {
            owner_bare_jid: alice(),
        };
        store.apply(&delete).await.expect("apply delete");
        assert!(store.get(&alice()).await.unwrap().is_none());
    }
}
