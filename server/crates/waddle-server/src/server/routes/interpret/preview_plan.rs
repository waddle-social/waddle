//! Freeze preview-reference writes from read-only database and plan snapshots.
use std::collections::BTreeMap;

use jid::BareJid;
use tracing::warn;
use uuid::Uuid;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, LinkPreviewMediaRefMutation, LinkPreviewMediaRefState},
    mam::RichMessageId,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

use super::{
    effects::{
        direct::{external, ExternalDirectEffect},
        Effect, ExternalEffect, PlanFailure,
    },
    Deps,
};
use crate::db::{actor::DbQuery, row_value, Value, ValueExt};

pub(super) async fn update(
    deps: &Deps<'_>,
    archive: &BareJid,
    message_id: &RichMessageId,
    archive_id: &StanzaId,
    message: &Message,
) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let mut mutations = current_references(deps, archive, message_id).await;
    for slot in crate::server::routes::websocket::link_preview_refs::cached_preview_upload_slot_ids(
        message,
        state.deps.auth_state.base_url.as_str(),
    ) {
        let Ok(upload_slot_id) = Uuid::parse_str(&slot) else {
            continue;
        };
        mutations.push(LinkPreviewMediaRefMutation {
            upload_slot_id,
            archive: archive.clone(),
            message_id: message_id.clone(),
            current_archive_stanza_id: archive_id.clone(),
            state: LinkPreviewMediaRefState::Current,
        });
    }
    record(deps, mutations, false);
}

pub(super) async fn clear(deps: &Deps<'_>, archive: &BareJid, message_id: &RichMessageId) {
    record(
        deps,
        current_references(deps, archive, message_id).await,
        true,
    );
}

fn record(deps: &Deps<'_>, mutations: Vec<LinkPreviewMediaRefMutation>, clearing: bool) {
    if mutations.is_empty() {
        return;
    }
    for mutation in &mutations {
        deps.capture_intent(IngressEffectIntent::LinkPreviewMediaRef {
            mutation: mutation.clone(),
        });
    }
    let effect = if clearing {
        ExternalDirectEffect::ClearLinkPreviewRefs { mutations }
    } else {
        ExternalDirectEffect::LinkPreviewRefs { mutations }
    };
    external(deps, effect);
}

async fn current_references(
    deps: &Deps<'_>,
    archive: &BareJid,
    message_id: &RichMessageId,
) -> Vec<LinkPreviewMediaRefMutation> {
    let mut refs = stored_references(deps, archive, message_id).await;
    // A correction/retraction later in the same plan observes preceding planned refs.
    for planned in deps.effects.snapshot() {
        let Effect::External(ExternalEffect::Direct(effect)) = planned.effect else {
            continue;
        };
        let (ExternalDirectEffect::LinkPreviewRefs { mutations }
        | ExternalDirectEffect::ClearLinkPreviewRefs { mutations }) = effect
        else {
            continue;
        };
        for mutation in mutations {
            if mutation.archive != *archive || mutation.message_id != *message_id {
                continue;
            }
            match mutation.state {
                LinkPreviewMediaRefState::Current => {
                    refs.insert(mutation.upload_slot_id, mutation.current_archive_stanza_id);
                }
                LinkPreviewMediaRefState::Unreferenced => {
                    refs.remove(&mutation.upload_slot_id);
                }
            }
        }
    }
    refs.into_iter()
        .map(
            |(upload_slot_id, current_archive_stanza_id)| LinkPreviewMediaRefMutation {
                upload_slot_id,
                archive: archive.clone(),
                message_id: message_id.clone(),
                current_archive_stanza_id,
                state: LinkPreviewMediaRefState::Unreferenced,
            },
        )
        .collect()
}

async fn stored_references(
    deps: &Deps<'_>,
    archive: &BareJid,
    message_id: &RichMessageId,
) -> BTreeMap<Uuid, StanzaId> {
    let Some(state) = deps.web_socket_state else {
        return BTreeMap::new();
    };
    let rows = match state.deps.app_state.db_pool.global_actor().ask(DbQuery {
        sql: "SELECT upload_slot_id, current_archive_id FROM link_preview_media_refs WHERE archive_jid = ? AND message_id = ? AND state = ?".to_owned(),
        params: vec![Value::from(archive.to_string()), Value::from(message_id.as_str().to_owned()), Value::from("current".to_owned())],
    }).await {
        Ok(rows) => rows,
        Err(_) => {
            warn!("could not read preview references during planning");
            deps.effects.fail_plan(PlanFailure::PreviewReferenceRead);
            return BTreeMap::new();
        }
    };
    rows.into_iter()
        .filter_map(|row| {
            let slot = row_value(&row, 0).and_then(ValueExt::as_string).ok()?;
            let upload_slot_id = Uuid::parse_str(&slot).ok()?;
            let archive_id = row_value(&row, 1).and_then(ValueExt::as_string).ok()?;
            Some((
                upload_slot_id,
                StanzaId::new(archive_id, jid::Jid::from(archive.clone())),
            ))
        })
        .collect()
}

/// Execute the frozen mutations without re-reading mutable reference state.
pub(super) async fn execute(
    deps: &Deps<'_>,
    mutations: Vec<LinkPreviewMediaRefMutation>,
) -> super::effects::EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return super::effects::EffectOutcome::Unavailable;
    };
    for mutation in mutations {
        let query = mutation_write(
            &mutation,
            chrono::Utc::now(),
            state.deps.app_state.db_pool.global().driver(),
        );
        if state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(query)
            .await
            .is_err()
        {
            warn!("planned preview reference write failed");
            return super::effects::EffectOutcome::Unavailable;
        }
    }
    super::effects::EffectOutcome::Completed
}

/// Frozen projection write shared by standalone and canonical transaction paths.
/// Canonical callers pass the original acceptance time, never retry wall time.
pub(crate) fn mutation_write(
    mutation: &LinkPreviewMediaRefMutation,
    now: chrono::DateTime<chrono::Utc>,
    driver: crate::db::DatabaseDriver,
) -> crate::db::actor::DbExecute {
    use crate::db::actor::DbExecute;
    let now = now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let mut query = match mutation.state {
        LinkPreviewMediaRefState::Current => DbExecute {
            sql: r#"
                INSERT INTO link_preview_media_refs
                    (upload_slot_id, archive_jid, message_id, current_archive_id,
                     state, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (upload_slot_id, archive_jid, message_id) DO UPDATE SET
                    current_archive_id = excluded.current_archive_id,
                    state = excluded.state,
                    updated_at = excluded.updated_at
                WHERE link_preview_media_refs.updated_at <= excluded.updated_at
                  AND (
                    link_preview_media_refs.updated_at < excluded.updated_at
                    OR link_preview_media_refs.current_archive_id = excluded.current_archive_id
                    OR EXISTS (
                        SELECT 1 FROM mam_messages current_revision
                        JOIN mam_messages proposed_revision
                          ON current_revision.room_jid = proposed_revision.room_jid
                        WHERE current_revision.room_jid = link_preview_media_refs.archive_jid
                          AND current_revision.id = link_preview_media_refs.current_archive_id
                          AND proposed_revision.id = excluded.current_archive_id
                          AND current_revision.archive_seq < proposed_revision.archive_seq
                    )
                  )
                  AND (link_preview_media_refs.state <> 'unreferenced'
                       OR link_preview_media_refs.current_archive_id <> excluded.current_archive_id)
                  AND NOT EXISTS (
                    SELECT 1 FROM mam_messages newer JOIN mam_messages proposed
                      ON newer.room_jid = proposed.room_jid
                    WHERE newer.room_jid = link_preview_media_refs.archive_jid
                      AND newer.id = link_preview_media_refs.current_archive_id
                      AND proposed.id = excluded.current_archive_id
                      AND newer.archive_seq > proposed.archive_seq
                  )
            "#
            .to_owned(),
            params: crate::db_params![
                mutation.upload_slot_id.to_string(),
                mutation.archive.to_string(),
                mutation.message_id.as_str().to_owned(),
                mutation.current_archive_stanza_id.id.clone(),
                "current".to_owned(),
                now.clone(),
                now,
            ],
        },
        LinkPreviewMediaRefState::Unreferenced => DbExecute {
            sql: r#"
                UPDATE link_preview_media_refs SET state = ?, updated_at = ?
                WHERE upload_slot_id = ? AND archive_jid = ? AND message_id = ?
                  AND current_archive_id = ? AND state = ? AND updated_at <= ?
            "#
            .to_owned(),
            params: crate::db_params![
                "unreferenced".to_owned(),
                now.clone(),
                mutation.upload_slot_id.to_string(),
                mutation.archive.to_string(),
                mutation.message_id.as_str().to_owned(),
                mutation.current_archive_stanza_id.id.clone(),
                "current".to_owned(),
                now,
            ],
        },
    };
    match driver {
        crate::db::DatabaseDriver::Sqlite => {
            query.sql = query.sql.replace("link_preview_media_refs.updated_at <= excluded.updated_at", "julianday(link_preview_media_refs.updated_at) <= julianday(excluded.updated_at)").replace("link_preview_media_refs.updated_at < excluded.updated_at", "julianday(link_preview_media_refs.updated_at) < julianday(excluded.updated_at)").replace("updated_at <= ?", "julianday(updated_at) <= julianday(?)");
        }
        crate::db::DatabaseDriver::Postgres => {
            query.sql = query.sql.replace("link_preview_media_refs.updated_at <= excluded.updated_at", "link_preview_media_refs.updated_at::timestamptz <= excluded.updated_at::timestamptz").replace("link_preview_media_refs.updated_at < excluded.updated_at", "link_preview_media_refs.updated_at::timestamptz < excluded.updated_at::timestamptz").replace("updated_at <= ?", "updated_at::timestamptz <= ?::timestamptz");
        }
    }
    query
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::IngressEffectCapture;
    use crate::server::routes::interpret::effects::PlanSink;
    use waddle_xmpp::registry::ConnectionRegistry;

    async fn preview_read_failure(
        fixture: crate::ingress::test_support::IngressFixture,
        correction: bool,
    ) {
        use crate::db::{DatabaseConfig, DatabasePool, PoolConfig};
        use crate::ingress::{commit::commit_submission, IngressDecisionClass};
        use crate::server::routes::interpret::message_plan::finish_plan;
        use crate::server::routes::websocket::tests::{
            create_test_websocket_state, create_test_websocket_state_with_db_pool_and_ingress,
        };
        use std::sync::Arc;

        let pool = Arc::new(
            DatabasePool::new(
                DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
                PoolConfig,
            )
            .await
            .expect("database pool"),
        );
        let standalone = create_test_websocket_state().await;
        let state = create_test_websocket_state_with_db_pool_and_ingress(
            pool,
            Arc::clone(&standalone.deps.protocol.ingress),
        )
        .await;
        let archive = fixture.principal.bare_jid().clone();
        let message_id = RichMessageId::new("original".to_owned()).expect("message id");
        let slot = Uuid::new_v4();
        fixture.execute(
            "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params![slot.to_string(), archive.to_string(), "preview.png".to_owned(), 1_i64, "image/png".to_owned(), "2099-01-01T00:00:00Z".to_owned()],
        ).await;
        fixture.execute(
            "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![slot.to_string(), archive.to_string(), message_id.as_str().to_owned(), "original-archive".to_owned(), "current".to_owned()],
        ).await;
        let registry = ConnectionRegistry::new();
        let sink = PlanSink::new();
        let capture = IngressEffectCapture::new();
        let deps = Deps {
            web_socket_state: Some(&state),
            effects: &sink,
            ..Deps::registry_only(&registry)
        }
        .with_ingress_effect_capture(Some(capture.clone()));
        assert_eq!(
            stored_references(&deps, &archive, &message_id).await.len(),
            1
        );
        fixture
            .execute(
                "ALTER TABLE link_preview_media_refs RENAME TO unavailable_preview_refs",
                (),
            )
            .await;
        let mut submission = fixture.submission(Some("preview-retry"), "changed");
        if correction {
            update(
                &deps,
                &archive,
                &message_id,
                &StanzaId::new("correction", archive.clone().into()),
                &submission.plan.sanitized_message,
            )
            .await;
        } else {
            clear(&deps, &archive, &message_id).await;
        }
        submission.plan = finish_plan(
            &sink,
            &capture,
            submission.plan.sanitized_message.clone(),
            Some(submission.sender.clone()),
        );
        assert_eq!(
            submission.plan.failure,
            Some(PlanFailure::PreviewReferenceRead)
        );
        assert!(submission.plan.plan.is_empty());
        assert!(submission.plan.intents.is_empty());
        fixture
            .execute(
                "ALTER TABLE unavailable_preview_refs RENAME TO link_preview_media_refs",
                (),
            )
            .await;
        let failure = commit_submission(&fixture.uow, &submission, 3)
            .await
            .expect_err("failed plan");
        assert_eq!(failure.class(), IngressDecisionClass::Storage);
        assert!(!failure.class().advances());
        for table in [
            "ingress_messages",
            "ingress_effect_intents",
            "ingress_effect_receipts",
            "ingress_sm_refs",
            "mam_messages",
        ] {
            assert_eq!(fixture.count(table).await, 0, "no writes to {table}");
        }
        assert_eq!(
            fixture
                .count("link_preview_media_refs WHERE state = 'current'")
                .await,
            1
        );
        // A healthy retry can now capture the obligation the failed read could not freeze.
        let retry_sink = PlanSink::new();
        let retry = Deps {
            effects: &retry_sink,
            ..deps.clone()
        };
        clear(&retry, &archive, &message_id).await;
        assert!(retry_sink.failure().is_none());
        assert_eq!(retry_sink.snapshot().len(), 1);
        drop(state);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_preview_correction_read_failure_is_nonadvancing_storage() {
        preview_read_failure(
            crate::ingress::test_support::IngressFixture::sqlite().await,
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn sqlite_preview_retraction_read_failure_is_nonadvancing_storage() {
        preview_read_failure(
            crate::ingress::test_support::IngressFixture::sqlite().await,
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn postgres_preview_correction_read_failure_is_nonadvancing_storage() {
        if let Some(fixture) =
            crate::ingress::test_support::IngressFixture::postgres("preview_correction").await
        {
            preview_read_failure(fixture, true).await;
        }
    }

    #[tokio::test]
    async fn postgres_preview_retraction_read_failure_is_nonadvancing_storage() {
        if let Some(fixture) =
            crate::ingress::test_support::IngressFixture::postgres("preview_retraction").await
        {
            preview_read_failure(fixture, false).await;
        }
    }

    #[tokio::test]
    async fn clear_freezes_prior_planned_references_and_captures_the_mutations() {
        let registry = ConnectionRegistry::new();
        let sink = PlanSink::new();
        let capture = IngressEffectCapture::new();
        let mut deps =
            Deps::registry_only(&registry).with_ingress_effect_capture(Some(capture.clone()));
        deps.effects = &sink;
        let archive: BareJid = "alice@example.com".parse().expect("archive");
        let message_id = RichMessageId::new("wire-1".to_owned()).expect("message id");
        let current = LinkPreviewMediaRefMutation {
            upload_slot_id: Uuid::nil(),
            archive: archive.clone(),
            message_id: message_id.clone(),
            current_archive_stanza_id: StanzaId::new("archive-1", jid::Jid::from(archive.clone())),
            state: LinkPreviewMediaRefState::Current,
        };
        record(&deps, vec![current.clone()], false);
        clear(&deps, &archive, &message_id).await;
        let mut expected = current;
        expected.state = LinkPreviewMediaRefState::Unreferenced;
        let plan = sink.snapshot();
        assert!(matches!(&plan[1].effect,
            Effect::External(ExternalEffect::Direct(ExternalDirectEffect::ClearLinkPreviewRefs { mutations })) if mutations == &vec![expected.clone()]));
        assert!(capture
            .snapshot()
            .intents
            .contains(&IngressEffectIntent::LinkPreviewMediaRef { mutation: expected }));
        clear(&deps, &archive, &message_id).await;
        assert_eq!(
            sink.snapshot().len(),
            2,
            "a second clear sees the virtual unreferenced state"
        );
    }
}
