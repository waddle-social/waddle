//! Durable XEP-0357 registration storage for the user XMPP server.
//!
//! This stores only the XMPP registration tuple and PubSub publish-options.
//! Provider credentials such as Web Push endpoints or APNS/FCM tokens belong
//! behind the XMPP Push Service boundary, not in the user-server registration
//! table.

use crate::db::DatabaseDriver;
use crate::db::Row;
use crate::db::Rows;
use crate::db::Transaction;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use minidom::Element;
use waddle_xmpp::push::{PushError, PushSubscription, PushSubscriptionStore};
use waddle_xmpp::xep::NS_DATA_FORMS;

use crate::db::{Database, IntoParams};

pub(crate) const STATUS_ENABLED: &str = "enabled";
pub(crate) const STATUS_DISABLED: &str = "disabled";
const PROVIDER_CREDENTIAL_FIELD_VARS: &[&str] = &[
    "endpoint",
    "p256dh",
    "auth",
    "service",
    "device-token",
    "device-key",
    "provider-endpoint",
    "provider-token",
    "provider-key-material",
    "apns-token",
    "fcm-token",
    "fcm-p256dh",
    "fcm-auth",
    "web-push-endpoint",
    "web-push-p256dh",
    "web-push-auth",
];

#[derive(Clone)]
pub struct DatabasePushRegistrationStore {
    db: Database,
}

/// Failures from the owner-only registration schema initializer.
#[derive(Debug, thiserror::Error)]
pub enum PushRegistrationOwnerError {
    #[error(transparent)]
    Storage(#[from] PushError),
    /// The original backend might still hold its transaction and catalog lock.
    #[error("push-registration initialization rollback was not acknowledged; reconcile the original backend before retrying")]
    RollbackUnconfirmed,
    /// An old table or index cannot establish whether this attempt committed.
    #[error("push-registration initialization COMMIT is indeterminate; reconcile the original backend and authoritative primary before retrying")]
    CommitIndeterminate,
}

impl DatabasePushRegistrationStore {
    pub async fn try_new(db: Database) -> Result<Self, PushRegistrationOwnerError> {
        Self::initialize_owner(db).await
    }

    /// Initialize the registration schema through this exact owner database
    /// handle. Existing callers retain the historical bootstrap behavior via
    /// [`Self::try_new`].
    pub async fn initialize_owner(db: Database) -> Result<Self, PushRegistrationOwnerError> {
        if db.attested_pg_writer_capability().is_some() {
            return Err(PushError::StorageError(
                "push-registration owner initialization cannot use an attested runtime handle"
                    .to_owned(),
            )
            .into());
        }
        Self::build_initialized(db).await
    }

    async fn build_initialized(db: Database) -> Result<Self, PushRegistrationOwnerError> {
        let store = Self { db };
        store.initialize().await?;
        Ok(store)
    }

    /// Attach to an owner-prepared PostgreSQL table through the exact
    /// physically attested main-pool handle. This path is read-only and does
    /// not reopen a URL or apply schema DDL.
    pub async fn attach_existing(
        db: Database,
    ) -> Result<Self, crate::notification_activity::DurableStoreAttachError> {
        crate::notification_activity::runtime_attach::verify_attested_table(
            &db,
            &crate::notification_activity::runtime_attach::PUSH_REGISTRATIONS_CONTRACT,
        )
        .await?;
        Ok(Self { db })
    }

    async fn initialize(&self) -> Result<(), PushRegistrationOwnerError> {
        if self.db.driver() == DatabaseDriver::Postgres {
            return self.initialize_postgres().await;
        }
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

    /// Keep both PostgreSQL DDL statements on the backend holding catalog-X.
    /// The fence coordinates only participating mutators; it does not replace
    /// operator-enforced quiescence of older or privileged writers.
    async fn initialize_postgres(&self) -> Result<(), PushRegistrationOwnerError> {
        const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(35);
        const END_TIMEOUT: Duration = Duration::from_secs(5);

        let mut tx = self
            .db
            .begin_catalog_fenced_with_idle_bound(&[])
            .await
            .map_err(|error| match error {
                crate::db::DatabaseError::CatalogMutationRollbackUnconfirmed => {
                    PushRegistrationOwnerError::RollbackUnconfirmed
                }
                other => PushError::StorageError(other.to_string()).into(),
            })?;
        let result = tokio::time::timeout(INITIALIZE_TIMEOUT, prepare_postgres(&mut tx)).await;
        let error = match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some(PushError::StorageError(
                "push-registration PostgreSQL initialization timed out".to_owned(),
            )),
        };
        if let Some(error) = error {
            if !tx.rollback_bounded_discard(END_TIMEOUT).await {
                return Err(PushRegistrationOwnerError::RollbackUnconfirmed);
            }
            return Err(error.into());
        }

        // An old successful schema cannot prove whether this particular COMMIT
        // completed. Discard an unacknowledged lease and return no store.
        if !matches!(
            tokio::time::timeout(END_TIMEOUT, tx.commit()).await,
            Ok(Ok(()))
        ) {
            return Err(PushRegistrationOwnerError::CommitIndeterminate);
        }
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

    async fn query(&self, sql: &str, params: impl IntoParams) -> Result<Rows, PushError> {
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

/// Capture the effective persistent schema under catalog-X, reject a temporary
/// table shadow, then use catalog-first name resolution and qualified DDL.
async fn prepare_postgres(tx: &mut Transaction<'_>) -> Result<(), PushError> {
    let connection = tx.postgres_connection().ok_or_else(|| {
        PushError::StorageError("push-registration initialization requires PostgreSQL".to_owned())
    })?;
    let schema: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.current_schema()::pg_catalog.text")
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| PushError::StorageError(error.to_string()))?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    for (setting, cap) in [("lock_timeout", "5s"), ("statement_timeout", "30s")] {
        sqlx::query(
            "SELECT pg_catalog.set_config($1, \
             CASE WHEN pg_catalog.current_setting($1)::pg_catalog.interval = '0'::pg_catalog.interval \
               OR pg_catalog.current_setting($1)::pg_catalog.interval > $2::pg_catalog.interval \
             THEN $2 ELSE pg_catalog.current_setting($1) END, true)",
        )
        .bind(setting)
        .bind(cap)
        .execute(&mut *connection)
        .await
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    }
    let schema = schema
        .filter(|schema| !schema.starts_with("pg_") && schema != "information_schema")
        .ok_or_else(|| {
            PushError::StorageError(
                "push-registration initialization requires a persistent application schema"
                    .to_owned(),
            )
        })?;
    let shadow: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class \
         WHERE relnamespace = pg_catalog.pg_my_temp_schema() \
           AND relname = 'push_registrations')",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| PushError::StorageError(error.to_string()))?;
    if shadow {
        return Err(PushError::StorageError(
            "push-registration initialization refused a temporary table shadow".to_owned(),
        ));
    }
    let table = format!("\"{}\".push_registrations", schema.replace('"', "\"\""));
    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (\
         owner_bare_jid TEXT NOT NULL, \
         push_service_jid TEXT NOT NULL, \
         node TEXT NOT NULL DEFAULT '', \
         publish_options_xml TEXT, \
         status TEXT NOT NULL CHECK (status IN ('enabled', 'disabled', 'backoff')), \
         failure_count INTEGER NOT NULL DEFAULT 0, \
         last_error TEXT, next_retry_at_ms BIGINT, \
         created_at_ms BIGINT NOT NULL, updated_at_ms BIGINT NOT NULL, \
         PRIMARY KEY (owner_bare_jid, push_service_jid, node))"
    ))
    .execute(&mut *connection)
    .await
    .map_err(|error| PushError::StorageError(error.to_string()))?;
    sqlx::query(&format!(
        "CREATE INDEX IF NOT EXISTS idx_push_registrations_owner_status \
         ON {table} (owner_bare_jid, status)"
    ))
    .execute(&mut *connection)
    .await
    .map_err(|error| PushError::StorageError(error.to_string()))?;
    Ok(())
}

impl PushSubscriptionStore for DatabasePushRegistrationStore {
    fn register(
        &self,
        sub: PushSubscription,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
        Box::pin(async move {
            register_subscription(&self.db, &sub).await?;
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

pub(crate) async fn register_subscription_tx(
    tx: &mut Transaction<'_>,
    sub: &PushSubscription,
) -> Result<(), PushError> {
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
    tx.execute(
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
            sub.user_jid.clone(),
            sub.service_jid.clone(),
            node,
            publish_options_xml,
            STATUS_ENABLED,
            now_ms,
            now_ms,
        ],
    )
    .await
    .map_err(|error| PushError::StorageError(error.to_string()))?;
    Ok(())
}

/// XEP-0357 §6 forward cleanup (#775): transition an ENABLED
/// (owner, service, node) registration to `disabled`, recording why.
/// Called by the publish-job worker inside ITS transaction when the
/// last active device on the node was disabled by a permanent
/// provider error (410 GONE et al.) — push service and user server
/// share a process and database, so the §6 "IQ-error back to the
/// publisher" reduces to this in-proc transition. Once disabled,
/// `get_for_user` (the outbound target resolver's source) stops
/// returning the row and no further candidates enqueue. Returns the
/// number of rows transitioned (0 = already disabled or unknown).
pub(crate) async fn disable_registration_tx(
    tx: &mut Transaction<'_>,
    owner_bare_jid: &jid::BareJid,
    service_jid: &jid::BareJid,
    node: &str,
    last_error: &str,
) -> Result<u64, PushError> {
    let now_ms = crate::time::now_ms();
    tx.execute(
        r#"
        UPDATE push_registrations
        SET status = ?,
            last_error = ?,
            updated_at_ms = ?
        WHERE owner_bare_jid = ? AND push_service_jid = ? AND node = ? AND status = ?
        "#,
        crate::db_params![
            STATUS_DISABLED,
            last_error,
            now_ms,
            owner_bare_jid.to_string(),
            service_jid.to_string(),
            node,
            STATUS_ENABLED,
        ],
    )
    .await
    .map_err(|error| PushError::StorageError(error.to_string()))
}

/// Explicit node retirement also stops user-server candidate generation. Keep
/// this in the caller's transaction with credential erasure and custody release.
/// Standalone push-service stores need not have a user-server registration table.
pub(crate) async fn disable_node_registrations_tx(
    tx: &mut Transaction<'_>,
    owner: &jid::BareJid,
    service: &jid::BareJid,
    node: Option<&str>,
) -> Result<u64, PushError> {
    if !registration_table_exists_tx(tx).await? {
        return Ok(0);
    }
    tx.execute(
        "UPDATE push_registrations SET status = ?, next_retry_at_ms = NULL, last_error = 'push node explicitly retired', updated_at_ms = ? WHERE owner_bare_jid = ? AND push_service_jid = ? AND (node = ? OR ? = 0) AND status <> ?",
        crate::db_params![STATUS_DISABLED, crate::time::now_ms(), owner.to_string(), service.to_string(), node, i64::from(node.is_some()), STATUS_DISABLED],
    ).await.map_err(|error| PushError::StorageError(error.to_string()))
}

pub(crate) async fn registration_table_exists_tx(
    tx: &mut Transaction<'_>,
) -> Result<bool, PushError> {
    let probe = if tx.driver() == DatabaseDriver::Postgres {
        "SELECT 1 WHERE to_regclass('push_registrations') IS NOT NULL"
    } else {
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'push_registrations'"
    };
    let mut rows = tx
        .query(probe, ())
        .await
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    let exists = rows
        .next()
        .await
        .map_err(|error| PushError::StorageError(error.to_string()))?
        .is_some();
    Ok(exists)
}

pub(crate) async fn registered_nodes_for_disable_tx(
    tx: &mut Transaction<'_>,
    owner_bare_jid: &jid::BareJid,
    service_jid: &str,
    node: Option<&str>,
) -> Result<Vec<String>, PushError> {
    let mut rows = if let Some(node) = node {
        tx.query(
            r#"
            SELECT node
            FROM push_registrations
            WHERE owner_bare_jid = ? AND push_service_jid = ? AND node = ?
            ORDER BY node ASC
            "#,
            crate::db_params![owner_bare_jid.to_string(), service_jid, node],
        )
        .await
    } else {
        tx.query(
            r#"
            SELECT node
            FROM push_registrations
            WHERE owner_bare_jid = ? AND push_service_jid = ?
            ORDER BY node ASC
            "#,
            crate::db_params![owner_bare_jid.to_string(), service_jid],
        )
        .await
    }
    .map_err(|error| PushError::StorageError(error.to_string()))?;

    let mut nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| PushError::StorageError(error.to_string()))?
    {
        nodes.push(
            row.get(0)
                .map_err(|error| PushError::StorageError(error.to_string()))?,
        );
    }
    Ok(nodes)
}

pub(crate) async fn remove_subscription_tx(
    tx: &mut Transaction<'_>,
    owner_bare_jid: &jid::BareJid,
    service_jid: &str,
    node: Option<&str>,
) -> Result<u64, PushError> {
    let removed = if let Some(node) = node {
        tx.execute(
            r#"
            DELETE FROM push_registrations
            WHERE owner_bare_jid = ? AND push_service_jid = ? AND node = ?
            "#,
            crate::db_params![owner_bare_jid.to_string(), service_jid, node],
        )
        .await
    } else {
        tx.execute(
            r#"
            DELETE FROM push_registrations
            WHERE owner_bare_jid = ? AND push_service_jid = ?
            "#,
            crate::db_params![owner_bare_jid.to_string(), service_jid],
        )
        .await
    }
    .map_err(|error| PushError::StorageError(error.to_string()))?;
    Ok(removed)
}

async fn register_subscription(db: &Database, sub: &PushSubscription) -> Result<(), PushError> {
    let mut tx = db
        .begin()
        .await
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    register_subscription_tx(&mut tx, sub).await?;
    tx.commit()
        .await
        .map_err(|error| PushError::StorageError(error.to_string()))?;
    Ok(())
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

fn decode_registration(row: &Row) -> Result<PushSubscription, PushError> {
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
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "secret")
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
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
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
        DatabasePushRegistrationStore::try_new(
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
            let store = DatabasePushRegistrationStore::try_new(db)
                .await
                .expect("store");
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
        let reopened = DatabasePushRegistrationStore::try_new(reopened_db)
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
            &publish_options_with_field("endpoint", "https://updates.push.example/abc")
        ));
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("p256dh", "secret")
        ));
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("auth", "secret")
        ));
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("device-token", "secret")
        ));
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("provider-endpoint", "https://push.example.com/endpoint")
        ));
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("provider-key-material", "secret")
        ));
        assert!(publish_options_contains_provider_credentials(
            &publish_options_with_field("WEB-PUSH-AUTH", "secret")
        ));
        for fcm_field in ["fcm-token", "fcm-p256dh", "fcm-auth"] {
            assert!(
                publish_options_contains_provider_credentials(&publish_options_with_field(
                    fcm_field, "secret"
                )),
                "{fcm_field} is an FCM provider credential"
            );
        }
        assert!(!publish_options_contains_provider_credentials(
            &publish_options("server-secret")
        ));
    }
}
