//! Rooms where an extension bot has posted.
//!
//! A bot holds a room occupancy only for one send, so the room's roster
//! cannot say which bots speak there. One row per (room, plugin) records it
//! for the room's XEP-0030 bot listing. Uninstall and room destruction
//! delete the rows; a ban filters them on read.

use jid::BareJid;
use waddle_extensions::PluginId;

use crate::db::{Database, DatabaseError};

pub(crate) async fn record(
    db: &Database,
    room: &BareJid,
    plugin: &PluginId,
) -> Result<(), DatabaseError> {
    db.guard()
        .await?
        .execute(
            "INSERT INTO extension_bot_rooms (room_jid, plugin_id) VALUES (?, ?) \
             ON CONFLICT (room_jid, plugin_id) DO NOTHING",
            crate::db_params![room.to_string(), plugin.as_str()],
        )
        .await?;
    Ok(())
}

pub(crate) async fn list(db: &Database, room: &BareJid) -> Result<Vec<PluginId>, DatabaseError> {
    let conn = db.guard().await?;
    let mut rows = conn
        .query(
            "SELECT plugin_id FROM extension_bot_rooms WHERE room_jid = ? ORDER BY plugin_id",
            crate::db_params![room.to_string()],
        )
        .await?;
    let mut plugins = Vec::new();
    while let Some(row) = rows.next().await? {
        let plugin: String = row.get(0)?;
        plugins.push(PluginId::new(plugin).map_err(|_| {
            DatabaseError::QueryFailed("invalid persisted extension plugin id".to_owned())
        })?);
    }
    Ok(plugins)
}

pub(crate) async fn delete_room(db: &Database, room: &BareJid) -> Result<(), DatabaseError> {
    db.guard()
        .await?
        .execute(
            "DELETE FROM extension_bot_rooms WHERE room_jid = ?",
            crate::db_params![room.to_string()],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_once_lists_and_deletes_by_room() {
        let db = Database::in_memory("extension-bot-rooms")
            .await
            .expect("database");
        crate::db::MigrationRunner::global()
            .run(&db)
            .await
            .expect("migrations");
        let room: BareJid = "lobby@muc.example.com".parse().expect("room");
        let other: BareJid = "other@muc.example.com".parse().expect("room");
        let polls = PluginId::new("polls").expect("plugin");
        let echo = PluginId::new("echo").expect("plugin");
        for (room, plugin) in [
            (&room, &polls),
            (&room, &polls),
            (&room, &echo),
            (&other, &polls),
        ] {
            record(&db, room, plugin).await.expect("record");
        }
        assert_eq!(
            list(&db, &room).await.expect("list"),
            vec![echo, polls.clone()]
        );
        delete_room(&db, &room).await.expect("delete");
        assert!(list(&db, &room).await.expect("list").is_empty());
        assert_eq!(list(&db, &other).await.expect("list"), vec![polls]);
    }
}
