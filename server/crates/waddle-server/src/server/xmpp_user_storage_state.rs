use kameo::actor::ActorRef;
use tracing::{debug, warn};
use waddle_xmpp::XmppError;
use waddle_xmpp::inbox::{InboxEntry, storage::InboxStorage};

use crate::db::actor::{DbActor, DbExecute, DbQueryOne};
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::{Database, Value, ValueExt, row_value};

pub(crate) async fn get_blocklist(
    db: Database,
    user_jid: &jid::BareJid,
) -> Result<Vec<String>, XmppError> {
    debug!(jid = %user_jid, "Getting blocklist");

    let storage = DatabaseBlockingStorage::new(db);
    storage.get_blocklist(user_jid).await.map_err(|e| {
        warn!(jid = %user_jid, error = %e, "Failed to get blocklist");
        XmppError::internal(format!("Database error: {}", e))
    })
}

pub(crate) async fn is_blocked(
    db: Database,
    user_jid: &jid::BareJid,
    blocked_jid: &jid::BareJid,
) -> Result<bool, XmppError> {
    debug!(user = %user_jid, blocked = %blocked_jid, "Checking if JID is blocked");

    let storage = DatabaseBlockingStorage::new(db);
    storage.is_blocked(user_jid, blocked_jid).await.map_err(|e| {
        warn!(user = %user_jid, blocked = %blocked_jid, error = %e, "Failed to check if blocked");
        XmppError::internal(format!("Database error: {}", e))
    })
}

pub(crate) async fn add_blocks(
    db: Database,
    user_jid: &jid::BareJid,
    blocked_jids: &[String],
) -> Result<usize, XmppError> {
    debug!(jid = %user_jid, count = blocked_jids.len(), "Adding blocks");

    let storage = DatabaseBlockingStorage::new(db);
    storage
        .add_blocks(user_jid, blocked_jids)
        .await
        .map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to add blocks");
            XmppError::internal(format!("Database error: {}", e))
        })
}

pub(crate) async fn remove_blocks(
    db: Database,
    user_jid: &jid::BareJid,
    blocked_jids: &[String],
) -> Result<usize, XmppError> {
    debug!(jid = %user_jid, count = blocked_jids.len(), "Removing blocks");

    let storage = DatabaseBlockingStorage::new(db);
    storage
        .remove_blocks(user_jid, blocked_jids)
        .await
        .map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to remove blocks");
            XmppError::internal(format!("Database error: {}", e))
        })
}

pub(crate) async fn remove_all_blocks(
    db: Database,
    user_jid: &jid::BareJid,
) -> Result<usize, XmppError> {
    debug!(jid = %user_jid, "Removing all blocks");

    let storage = DatabaseBlockingStorage::new(db);
    storage.remove_all_blocks(user_jid).await.map_err(|e| {
        warn!(jid = %user_jid, error = %e, "Failed to remove all blocks");
        XmppError::internal(format!("Database error: {}", e))
    })
}

pub(crate) async fn get_private_xml(
    global_db_actor: &ActorRef<DbActor>,
    jid: &jid::BareJid,
    namespace: &str,
) -> Result<Option<String>, XmppError> {
    debug!(jid = %jid, namespace = %namespace, "Getting private XML");

    let row = global_db_actor
        .ask(DbQueryOne {
            sql: "SELECT xml_content FROM private_xml_storage WHERE jid = ? AND namespace = ?"
                .to_string(),
            params: vec![
                Value::from(jid.to_string()),
                Value::from(namespace.to_string()),
            ],
        })
        .await
        .map_err(|e| {
            warn!(jid = %jid, namespace = %namespace, error = %e, "Failed to get private XML");
            XmppError::internal(format!("Database actor error: {}", e))
        })?;

    match row {
        Some(values) => row_value(&values, 0)
            .and_then(|value| value.as_string())
            .map(Some)
            .map_err(|e| XmppError::internal(format!("Database error: {}", e))),
        None => Ok(None),
    }
}

pub(crate) async fn set_private_xml(
    global_db_actor: &ActorRef<DbActor>,
    jid: &jid::BareJid,
    namespace: &str,
    xml_content: &str,
) -> Result<(), XmppError> {
    debug!(jid = %jid, namespace = %namespace, "Setting private XML");

    global_db_actor
        .ask(DbExecute {
            sql: "INSERT OR REPLACE INTO private_xml_storage (jid, namespace, xml_content, updated_at) VALUES (?, ?, ?, datetime('now'))".to_string(),
            params: vec![
                Value::from(jid.to_string()),
                Value::from(namespace.to_string()),
                Value::from(xml_content.to_string()),
            ],
        })
        .await
        .map_err(|e| {
            warn!(jid = %jid, namespace = %namespace, error = %e, "Failed to set private XML");
            XmppError::internal(format!("Database actor error: {}", e))
        })?;

    Ok(())
}

pub(crate) async fn list_inbox(
    storage: Option<&dyn InboxStorage>,
    user_jid: &jid::BareJid,
) -> Result<Vec<InboxEntry>, XmppError> {
    let storage = require_inbox_storage(storage)?;
    storage.list(user_jid).await.map_err(|error| {
        warn!(jid = %user_jid, error = %error, "Failed to list inbox");
        XmppError::internal(format!("Inbox error: {}", error))
    })
}

pub(crate) async fn upsert_inbox_entry(
    storage: Option<&dyn InboxStorage>,
    user_jid: &jid::BareJid,
    entry: InboxEntry,
    increment_unread: bool,
) -> Result<(), XmppError> {
    let storage = require_inbox_storage(storage)?;
    storage
        .upsert(user_jid, entry, increment_unread)
        .await
        .map(|_| ())
        .map_err(|error| {
            warn!(jid = %user_jid, error = %error, "Failed to upsert inbox entry");
            XmppError::internal(format!("Inbox error: {}", error))
        })
}

pub(crate) async fn mark_inbox_read(
    storage: Option<&dyn InboxStorage>,
    user_jid: &jid::BareJid,
    partner_jid: &jid::BareJid,
) -> Result<(), XmppError> {
    let storage = require_inbox_storage(storage)?;
    storage
        .mark_read(user_jid, partner_jid, None)
        .await
        .map_err(|error| {
            warn!(
                jid = %user_jid,
                partner = %partner_jid,
                error = %error,
                "Failed to mark inbox conversation read"
            );
            XmppError::internal(format!("Inbox error: {}", error))
        })
}

pub(crate) async fn inbox_total_unread(
    storage: Option<&dyn InboxStorage>,
    user_jid: &jid::BareJid,
) -> Result<u64, XmppError> {
    let storage = require_inbox_storage(storage)?;
    storage.total_unread(user_jid).await.map_err(|error| {
        warn!(jid = %user_jid, error = %error, "Failed to count inbox unread");
        XmppError::internal(format!("Inbox error: {}", error))
    })
}

fn require_inbox_storage(
    storage: Option<&dyn InboxStorage>,
) -> Result<&dyn InboxStorage, XmppError> {
    storage.ok_or_else(|| XmppError::internal("Inbox storage not configured"))
}
