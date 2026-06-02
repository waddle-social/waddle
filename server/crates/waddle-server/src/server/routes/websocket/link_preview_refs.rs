use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::warn;
use xmpp_parsers::message::Message;

use crate::db::{
    actor::{DbActor, DbExecute},
    Value,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkPreviewMediaRefState {
    Current,
    Unreferenced,
}

impl LinkPreviewMediaRefState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Unreferenced => "unreferenced",
        }
    }
}

pub(crate) async fn record_current_message_preview_refs(
    global_db_actor: &ActorRef<DbActor>,
    trusted_media_base_url: &str,
    archive_jid: &BareJid,
    message_id: &str,
    current_archive_id: &str,
    message: &Message,
) {
    clear_current_message_preview_refs(global_db_actor, archive_jid, message_id).await;

    for upload_slot_id in cached_preview_upload_slot_ids(message, trusted_media_base_url) {
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(error) = global_db_actor
            .ask(DbExecute {
                sql: "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (upload_slot_id, archive_jid, message_id) DO UPDATE SET current_archive_id = excluded.current_archive_id, state = excluded.state, updated_at = excluded.updated_at".to_string(),
                params: vec![
                    Value::from(upload_slot_id),
                    Value::from(archive_jid.to_string()),
                    Value::from(message_id.to_string()),
                    Value::from(current_archive_id.to_string()),
                    Value::from(LinkPreviewMediaRefState::Current.as_str().to_string()),
                    Value::from(now.clone()),
                    Value::from(now),
                ],
            })
            .await
        {
            warn!(%error, archive = %archive_jid, message_id, "failed to record link preview media ref");
        }
    }
}

pub(crate) async fn clear_current_message_preview_refs(
    global_db_actor: &ActorRef<DbActor>,
    archive_jid: &BareJid,
    message_id: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(error) = global_db_actor
        .ask(DbExecute {
            sql: "UPDATE link_preview_media_refs SET state = ?, updated_at = ? WHERE archive_jid = ? AND message_id = ? AND state = ?".to_string(),
            params: vec![
                Value::from(LinkPreviewMediaRefState::Unreferenced.as_str().to_string()),
                Value::from(now),
                Value::from(archive_jid.to_string()),
                Value::from(message_id.to_string()),
                Value::from(LinkPreviewMediaRefState::Current.as_str().to_string()),
            ],
        })
        .await
    {
        warn!(%error, archive = %archive_jid, message_id, "failed to clear link preview media refs");
    }
}

fn cached_preview_upload_slot_ids(message: &Message, trusted_media_base_url: &str) -> Vec<String> {
    message
        .payloads
        .iter()
        .filter(|payload| {
            payload.name() == "reference"
                && payload.ns() == waddle_xmpp::xep::NS_REFERENCE
                && payload.attr("type") == Some("data")
        })
        .filter_map(|payload| payload.attr("uri"))
        .filter_map(|uri| cached_preview_upload_slot_id(uri, trusted_media_base_url))
        .collect()
}

fn cached_preview_upload_slot_id(uri: &str, trusted_media_base_url: &str) -> Option<String> {
    let url = url::Url::parse(uri).ok()?;
    let trusted = url::Url::parse(trusted_media_base_url).ok()?;
    if url.scheme() != trusted.scheme()
        || url.host_str() != trusted.host_str()
        || url.port_or_known_default() != trusted.port_or_known_default()
    {
        return None;
    }
    let mut parts = url.path().strip_prefix("/api/files/")?.split('/');
    let slot_id = parts.next()?;
    let filename = parts.next()?;
    if parts.next().is_some()
        || uuid::Uuid::parse_str(slot_id).is_err()
        || !is_cached_preview_filename(filename)
    {
        return None;
    }
    Some(slot_id.to_string())
}

fn is_cached_preview_filename(filename: &str) -> bool {
    let Some(rest) = filename.strip_prefix("link-preview-") else {
        return false;
    };
    let Some((hash, extension)) = rest.rsplit_once('.') else {
        return false;
    };
    hash.len() == 64
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(extension, "png" | "jpg" | "gif" | "webp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        actor::{DbExecute, DbQueryOne},
        DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig, ValueExt,
    };

    async fn db_pool() -> DatabasePool {
        let pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(pool.global())
            .await
            .expect("migrations");
        pool
    }

    async fn seed_uploaded_slot(pool: &DatabasePool, slot_id: &str) {
        pool.global_actor()
            .ask(DbExecute {
                sql: "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, storage_key, expires_at, uploaded_at) VALUES (?, ?, ?, ?, ?, 'uploaded', ?, ?, ?)".to_string(),
                params: crate::db_params![
                    slot_id,
                    "alice@example.com",
                    "link-preview-test.png",
                    12_i64,
                    "image/png",
                    "link-previews/sha256/test",
                    "2030-01-01T00:00:00Z",
                    "2026-01-01T00:00:00Z",
                ],
            })
            .await
            .expect("seed slot");
    }

    fn message_with_preview_ref(slot_id: &str) -> Message {
        let to: BareJid = "bob@example.com".parse().expect("jid");
        let mut message = Message::new(Some(jid::Jid::from(to)));
        let hash = "86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16";
        waddle_xmpp::xep::add_reference(
            &mut message,
            &waddle_xmpp::xep::Reference::data(format!(
                "https://waddle.example/api/files/{slot_id}/link-preview-{hash}.png"
            )),
        );
        message
    }

    async fn ref_state(pool: &DatabasePool, slot_id: &str) -> Option<String> {
        let row = pool
            .global_actor()
            .ask(DbQueryOne {
                sql: "SELECT state FROM link_preview_media_refs WHERE upload_slot_id = ?"
                    .to_string(),
                params: crate::db_params![slot_id],
            })
            .await
            .expect("query");
        row.map(|values| values[0].as_string().expect("state").to_string())
    }

    async fn ref_archive_id(pool: &DatabasePool, slot_id: &str) -> Option<String> {
        let row = pool
            .global_actor()
            .ask(DbQueryOne {
                sql: "SELECT current_archive_id FROM link_preview_media_refs WHERE upload_slot_id = ?"
                    .to_string(),
                params: crate::db_params![slot_id],
            })
            .await
            .expect("query");
        row.map(|values| values[0].as_string().expect("archive id").to_string())
    }

    #[tokio::test]
    async fn correction_without_preview_marks_old_cached_media_unreferenced() {
        let pool = db_pool().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        seed_uploaded_slot(&pool, &slot_id).await;
        let archive: BareJid = "alice@example.com".parse().expect("archive");

        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "msg-1",
            "archive-1",
            &message_with_preview_ref(&slot_id),
        )
        .await;
        assert_eq!(ref_state(&pool, &slot_id).await.as_deref(), Some("current"));

        let corrected_to: BareJid = "bob@example.com".parse().expect("jid");
        let corrected_without_link = Message::new(Some(jid::Jid::from(corrected_to)));
        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "msg-1",
            "archive-2",
            &corrected_without_link,
        )
        .await;

        assert_eq!(
            ref_state(&pool, &slot_id).await.as_deref(),
            Some("unreferenced")
        );
    }

    #[tokio::test]
    async fn retraction_marks_cached_media_unreferenced() {
        let pool = db_pool().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        seed_uploaded_slot(&pool, &slot_id).await;
        let archive: BareJid = "alice@example.com".parse().expect("archive");

        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "msg-1",
            "archive-1",
            &message_with_preview_ref(&slot_id),
        )
        .await;
        clear_current_message_preview_refs(pool.global_actor(), &archive, "msg-1").await;

        assert_eq!(
            ref_state(&pool, &slot_id).await.as_deref(),
            Some("unreferenced")
        );
    }

    #[tokio::test]
    async fn deletion_marks_cached_media_unreferenced() {
        let pool = db_pool().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        seed_uploaded_slot(&pool, &slot_id).await;
        let archive: BareJid = "room@muc.example.com".parse().expect("archive");

        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "stanza-1",
            "archive-1",
            &message_with_preview_ref(&slot_id),
        )
        .await;
        clear_current_message_preview_refs(pool.global_actor(), &archive, "stanza-1").await;

        assert_eq!(
            ref_state(&pool, &slot_id).await.as_deref(),
            Some("unreferenced")
        );
    }

    #[tokio::test]
    async fn unchanged_message_keeps_current_cached_media_reference() {
        let pool = db_pool().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        seed_uploaded_slot(&pool, &slot_id).await;
        let archive: BareJid = "alice@example.com".parse().expect("archive");

        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "msg-1",
            "archive-1",
            &message_with_preview_ref(&slot_id),
        )
        .await;
        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "msg-1",
            "archive-1",
            &message_with_preview_ref(&slot_id),
        )
        .await;

        assert_eq!(ref_state(&pool, &slot_id).await.as_deref(), Some("current"));
        assert_eq!(
            ref_archive_id(&pool, &slot_id).await.as_deref(),
            Some("archive-1")
        );
    }

    #[tokio::test]
    async fn correction_with_new_preview_replaces_current_ref_for_message() {
        let pool = db_pool().await;
        let old_slot_id = uuid::Uuid::new_v4().to_string();
        let new_slot_id = uuid::Uuid::new_v4().to_string();
        seed_uploaded_slot(&pool, &old_slot_id).await;
        seed_uploaded_slot(&pool, &new_slot_id).await;
        let archive: BareJid = "alice@example.com".parse().expect("archive");

        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "msg-1",
            "archive-1",
            &message_with_preview_ref(&old_slot_id),
        )
        .await;
        record_current_message_preview_refs(
            pool.global_actor(),
            "https://waddle.example",
            &archive,
            "msg-1",
            "archive-2",
            &message_with_preview_ref(&new_slot_id),
        )
        .await;

        assert_eq!(
            ref_state(&pool, &old_slot_id).await.as_deref(),
            Some("unreferenced")
        );
        assert_eq!(
            ref_state(&pool, &new_slot_id).await.as_deref(),
            Some("current")
        );
        assert_eq!(
            ref_archive_id(&pool, &new_slot_id).await.as_deref(),
            Some("archive-2")
        );
    }

    #[test]
    fn cached_preview_upload_slot_id_requires_valid_preview_filename() {
        let slot_id = uuid::Uuid::new_v4().to_string();
        let hash = "86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16";
        let valid = format!("https://waddle.example/api/files/{slot_id}/link-preview-{hash}.webp");
        assert_eq!(
            cached_preview_upload_slot_id(&valid, "https://waddle.example").as_deref(),
            Some(slot_id.as_str())
        );

        let loose = format!("https://waddle.example/api/files/{slot_id}/link-preview-test.png");
        assert_eq!(
            cached_preview_upload_slot_id(&loose, "https://waddle.example"),
            None
        );
        let bad_extension =
            format!("https://waddle.example/api/files/{slot_id}/link-preview-{hash}.svg");
        assert_eq!(
            cached_preview_upload_slot_id(&bad_extension, "https://waddle.example"),
            None
        );
    }
}
