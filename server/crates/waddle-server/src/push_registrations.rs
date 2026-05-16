//! Durable XEP-0357 registration storage for the user XMPP server.
//!
//! This stores only the XMPP registration tuple and PubSub publish-options.
//! Provider credentials such as Web Push endpoints or APNS/FCM tokens belong
//! behind the XMPP Push Service boundary, not in the user-server registration
//! table.

use std::future::Future;
use std::pin::Pin;

use minidom::Element;
use waddle_xmpp::push::{PushError, PushSubscription, PushSubscriptionStore};
use waddle_xmpp::xep::NS_DATA_FORMS;

use crate::db::{Database, IntoParams};

const STATUS_ENABLED: &str = "enabled";
const PROVIDER_CREDENTIAL_FIELD_VARS: &[&str] = &[
    "endpoint",
    "p256dh",
    "auth",
    "service",
    "device-token",
    "device-key",
    "provider-token",
    "apns-token",
    "fcm-token",
    "web-push-endpoint",
    "web-push-p256dh",
    "web-push-auth",
];

#[derive(Clone)]
pub struct DatabasePushRegistrationStore {
    db: Database,
}

impl DatabasePushRegistrationStore {
    pub async fn new(db: Database) -> Result<Self, PushError> {
        let store = Self { db };
        store.initialize().await?;
        Ok(store)
    }

    async fn initialize(&self) -> Result<(), PushError> {
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS push_registrations (
                    owner_bare_jid TEXT NOT NULL,
                    push_service_jid TEXT NOT NULL,
                    node TEXT NOT NULL DEFAULT '',
                    publish_options_xml TEXT,
                    status TEXT NOT NULL CHECK (status IN ('enabled', 'disabled', 'backoff')),
                    failure_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    next_retry_at_ms {i64_type},
                    created_at_ms {i64_type} NOT NULL,
                    updated_at_ms {i64_type} NOT NULL,
                    PRIMARY KEY (owner_bare_jid, push_service_jid, node)
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_registrations_owner_status \
             ON push_registrations (owner_bare_jid, status)",
            (),
        )
        .await?;
        Ok(())
    }

    async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, PushError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| PushError::StorageError(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| PushError::StorageError(error.to_string()))
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, PushError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| PushError::StorageError(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| PushError::StorageError(error.to_string()))
    }
}

impl PushSubscriptionStore for DatabasePushRegistrationStore {
    fn register(
        &self,
        sub: PushSubscription,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
        Box::pin(async move {
            if sub
                .publish_options
                .as_ref()
                .is_some_and(publish_options_contains_provider_credentials)
            {
                return Err(PushError::StorageError(
                    "provider credential fields are not allowed in durable XEP-0357 registrations"
                        .to_string(),
                ));
            }
            let now_ms = crate::time::now_ms();
            let node = sub.node.clone().unwrap_or_default();
            let publish_options_xml = sub.publish_options.as_ref().map(String::from);
            self.execute(
                r#"
                INSERT INTO push_registrations (
                    owner_bare_jid,
                    push_service_jid,
                    node,
                    publish_options_xml,
                    status,
                    failure_count,
                    last_error,
                    next_retry_at_ms,
                    created_at_ms,
                    updated_at_ms
                ) VALUES (?, ?, ?, ?, ?, 0, NULL, NULL, ?, ?)
                ON CONFLICT(owner_bare_jid, push_service_jid, node) DO UPDATE SET
                    publish_options_xml = excluded.publish_options_xml,
                    status = excluded.status,
                    failure_count = 0,
                    last_error = NULL,
                    next_retry_at_ms = NULL,
                    updated_at_ms = excluded.updated_at_ms
                "#,
                crate::db_params![
                    sub.user_jid,
                    sub.service_jid,
                    node,
                    publish_options_xml,
                    STATUS_ENABLED,
                    now_ms,
                    now_ms,
                ],
            )
            .await?;
            Ok(())
        })
    }

    fn remove(
        &self,
        user_jid: &str,
        service_jid: &str,
        node: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
        let user_jid = user_jid.to_owned();
        let service_jid = service_jid.to_owned();
        let node = node.map(str::to_owned);
        Box::pin(async move {
            if let Some(node) = node {
                self.execute(
                    "DELETE FROM push_registrations \
                     WHERE owner_bare_jid = ? AND push_service_jid = ? AND node = ?",
                    crate::db_params![user_jid, service_jid, node],
                )
                .await?;
            } else {
                self.execute(
                    "DELETE FROM push_registrations \
                     WHERE owner_bare_jid = ? AND push_service_jid = ?",
                    crate::db_params![user_jid, service_jid],
                )
                .await?;
            }
            Ok(())
        })
    }

    fn get_for_user(
        &self,
        user_jid: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PushSubscription>, PushError>> + Send + '_>> {
        let user_jid = user_jid.to_owned();
        Box::pin(async move {
            let mut rows = self
                .query(
                    r#"
                    SELECT owner_bare_jid, push_service_jid, node, publish_options_xml
                    FROM push_registrations
                    WHERE owner_bare_jid = ? AND status = ?
                    ORDER BY push_service_jid ASC, node ASC
                    "#,
                    crate::db_params![user_jid, STATUS_ENABLED],
                )
                .await?;
            let mut registrations = Vec::new();
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| PushError::StorageError(error.to_string()))?
            {
                registrations.push(decode_registration(&row)?);
            }
            Ok(registrations)
        })
    }
}

pub(crate) fn publish_options_contains_provider_credentials(form: &Element) -> bool {
    form.children()
        .filter(|child| child.name() == "field" && child.ns() == NS_DATA_FORMS)
        .filter_map(|field| field.attr("var"))
        .any(|var| {
            PROVIDER_CREDENTIAL_FIELD_VARS
                .iter()
                .any(|disallowed| var.eq_ignore_ascii_case(disallowed))
        })
}

fn decode_registration(row: &crate::db::Row) -> Result<PushSubscription, PushError> {
    let user_jid: String = row
        .get(0)
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    let service_jid: String = row
        .get(1)
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    let node: String = row
        .get(2)
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    let publish_options_xml: Option<String> = row
        .get(3)
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    let publish_options = publish_options_xml
        .map(|xml| {
            xml.parse::<Element>()
                .map_err(|error| PushError::StorageError(error.to_string()))
        })
        .transpose()?;

    Ok(PushSubscription {
        user_jid,
        service_jid,
        node: if node.is_empty() { None } else { Some(node) },
        publish_options,
        endpoint: None,
        p256dh: None,
        auth_key: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn publish_options(secret: &str) -> Element {
        Element::builder("x", NS_DATA_FORMS)
            .attr("type", "submit")
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "FORM_TYPE")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "secret")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append(secret)
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    fn publish_options_with_field(var: &str, value: &str) -> Element {
        Element::builder("x", NS_DATA_FORMS)
            .attr("type", "submit")
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "FORM_TYPE")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", var)
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append(value)
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    fn registration(
        user: &str,
        service: &str,
        node: Option<&str>,
        secret: &str,
    ) -> PushSubscription {
        PushSubscription {
            user_jid: user.to_string(),
            service_jid: service.to_string(),
            node: node.map(str::to_string),
            publish_options: Some(publish_options(secret)),
            endpoint: Some("https://updates.push.services.mozilla.com/legacy".to_string()),
            p256dh: Some("legacy-key".to_string()),
            auth_key: Some("legacy-auth".to_string()),
        }
    }

    async fn memory_store() -> DatabasePushRegistrationStore {
        DatabasePushRegistrationStore::new(
            Database::in_memory("push-registrations")
                .await
                .expect("database"),
        )
        .await
        .expect("store")
    }

    #[tokio::test]
    async fn enable_upserts_same_service_node_and_preserves_publish_options() {
        let store = memory_store().await;
        store
            .register(registration(
                "alice@example.com",
                "push.example.com",
                Some("n1"),
                "old",
            ))
            .await
            .expect("register old");
        store
            .register(registration(
                "alice@example.com",
                "push.example.com",
                Some("n1"),
                "new",
            ))
            .await
            .expect("register new");

        let registrations = store
            .get_for_user("alice@example.com")
            .await
            .expect("registrations");

        assert_eq!(registrations.len(), 1);
        let options_xml = String::from(registrations[0].publish_options.as_ref().expect("options"));
        assert!(options_xml.contains("new"));
        assert!(!options_xml.contains("old"));
    }

    #[tokio::test]
    async fn disable_with_node_removes_only_that_service_node() {
        let store = memory_store().await;
        store
            .register(registration(
                "alice@example.com",
                "push.example.com",
                Some("n1"),
                "one",
            ))
            .await
            .expect("n1");
        store
            .register(registration(
                "alice@example.com",
                "push.example.com",
                Some("n2"),
                "two",
            ))
            .await
            .expect("n2");

        store
            .remove("alice@example.com", "push.example.com", Some("n1"))
            .await
            .expect("remove n1");

        let registrations = store
            .get_for_user("alice@example.com")
            .await
            .expect("registrations");
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].node.as_deref(), Some("n2"));
    }

    #[tokio::test]
    async fn disable_without_node_removes_all_nodes_for_service() {
        let store = memory_store().await;
        store
            .register(registration(
                "alice@example.com",
                "push.example.com",
                Some("n1"),
                "one",
            ))
            .await
            .expect("n1");
        store
            .register(registration(
                "alice@example.com",
                "push.example.com",
                Some("n2"),
                "two",
            ))
            .await
            .expect("n2");
        store
            .register(registration(
                "alice@example.com",
                "other.example.com",
                Some("n1"),
                "other",
            ))
            .await
            .expect("other");

        store
            .remove("alice@example.com", "push.example.com", None)
            .await
            .expect("remove service");

        let registrations = store
            .get_for_user("alice@example.com")
            .await
            .expect("registrations");
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].service_jid, "other.example.com");
    }

    #[tokio::test]
    async fn registrations_survive_store_reopen() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("push.sqlite3");
        {
            let db = Database::open_local("push-restart", &path)
                .await
                .expect("database");
            let store = DatabasePushRegistrationStore::new(db).await.expect("store");
            store
                .register(registration(
                    "alice@example.com",
                    "push.example.com",
                    Some("n1"),
                    "persisted",
                ))
                .await
                .expect("register");
        }

        let reopened_db = Database::open_local("push-restart-reopened", &path)
            .await
            .expect("reopened database");
        let reopened = DatabasePushRegistrationStore::new(reopened_db)
            .await
            .expect("reopened store");
        let registrations = reopened
            .get_for_user("alice@example.com")
            .await
            .expect("registrations");
        assert_eq!(registrations.len(), 1);
        let options_xml = String::from(registrations[0].publish_options.as_ref().expect("options"));
        assert!(options_xml.contains("persisted"));
    }

    #[tokio::test]
    async fn schema_does_not_persist_provider_credentials() {
        let store = memory_store().await;
        store
            .register(registration(
                "alice@example.com",
                "push.example.com",
                Some("n1"),
                "server-secret",
            ))
            .await
            .expect("register");

        let db = store.db.clone();
        let conn = db.guard().await.expect("db");
        let mut columns = conn
            .query("PRAGMA table_info(push_registrations)", ())
            .await
            .expect("columns");
        let mut names = Vec::new();
        while let Some(row) = columns.next().await.expect("row") {
            names.push(row.get::<String>(1).expect("name"));
        }

        assert!(!names.iter().any(|name| {
            matches!(
                name.as_str(),
                "endpoint" | "p256dh" | "auth" | "auth_key" | "provider_token"
            )
        }));

        let registrations = store
            .get_for_user("alice@example.com")
            .await
            .expect("registrations");
        assert_eq!(registrations.len(), 1);
        assert!(registrations[0].endpoint.is_none());
        assert!(registrations[0].p256dh.is_none());
        assert!(registrations[0].auth_key.is_none());
    }

    #[tokio::test]
    async fn register_rejects_provider_credentials_in_publish_options() {
        let store = memory_store().await;
        let mut sub = registration(
            "alice@example.com",
            "push.example.com",
            Some("n1"),
            "server-secret",
        );
        sub.publish_options = Some(publish_options_with_field(
            "web-push-endpoint",
            "https://updates.push.services.mozilla.com/abc",
        ));

        let err = store.register(sub).await.expect_err("reject provider data");
        assert!(err.to_string().contains("provider credential fields"));

        let registrations = store
            .get_for_user("alice@example.com")
            .await
            .expect("registrations");
        assert!(registrations.is_empty());
    }

    #[test]
    fn detects_provider_credential_publish_option_fields() {
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("device-token", "secret")
        ));
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("WEB-PUSH-AUTH", "secret")
        ));
        assert!(!publish_options_contains_provider_credentials(
            &publish_options("server-secret")
        ));
    }
}
