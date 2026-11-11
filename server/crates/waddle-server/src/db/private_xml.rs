//! XEP-0049 private XML storage statements.

use jid::BareJid;

use super::actor::DbExecute;
use super::Value;

/// Store one private XML element, replacing the stored element for the same
/// JID and namespace. The statement runs unchanged on SQLite and PostgreSQL:
/// the timestamp is bound as text in the form both column defaults produce,
/// and the conflict target is the table's primary key.
pub(crate) fn upsert(jid: &BareJid, namespace: &str, xml_content: &str) -> DbExecute {
    DbExecute {
        sql: "INSERT INTO private_xml_storage (jid, namespace, xml_content, updated_at) \
              VALUES (?, ?, ?, ?) \
              ON CONFLICT (jid, namespace) DO UPDATE SET \
                  xml_content = excluded.xml_content, \
                  updated_at = excluded.updated_at"
            .to_string(),
        params: vec![
            Value::from(jid.to_string()),
            Value::from(namespace.to_string()),
            Value::from(xml_content.to_string()),
            Value::from(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::upsert;
    use crate::db::{Database, MigrationRunner};

    #[tokio::test]
    async fn upsert_replaces_the_stored_element_on_sqlite() {
        let database = Database::in_memory("private-xml-upsert")
            .await
            .expect("in-memory SQLite database");
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("apply migrations");
        let jid: jid::BareJid = "alice@example.test".parse().expect("bare JID");
        let connection = database.guard().await.expect("SQLite connection");
        for content in ["<first/>", "<second/>"] {
            let statement = upsert(&jid, "urn:example:prefs", content);
            connection
                .execute(&statement.sql, statement.params)
                .await
                .expect("store private XML");
        }
        let mut rows = connection
            .query(
                "SELECT xml_content FROM private_xml_storage WHERE jid = ? AND namespace = ?",
                crate::db_params![jid.to_string(), "urn:example:prefs"],
            )
            .await
            .expect("read private XML");
        let stored: String = rows
            .next()
            .await
            .expect("row result")
            .expect("stored row")
            .get(0)
            .expect("stored content");
        assert_eq!(stored, "<second/>");
        assert!(rows.next().await.expect("row result").is_none());
    }
}
