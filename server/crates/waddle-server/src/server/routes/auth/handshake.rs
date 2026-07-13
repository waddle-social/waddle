//! Durable OIDC/OAuth handshake state (#1336).
//!
//! `pending_auth`, `device_auth`, and `xmpp_auth_codes` used to live in
//! per-process `DashMap`s, which breaks behind a load balancer: the pod
//! that mints a `state`/code is not necessarily the pod that redeems
//! it. This store keeps the same entries in the shared global database
//! (via the `DbActor`, like `SessionManager`) so any replica can serve
//! the callback / token / device endpoints.
//!
//! Redemption is single-use across replicas: `take_*` gates on the
//! rows-affected count of the `DELETE`, so exactly one caller wins even
//! when two pods race on the same key.

use super::*;

use crate::auth::AuthError;
use crate::db::actor::{DbActor, DbExecute, DbQueryOne};
use crate::db::{row_value, ValueExt};

/// Per-sweep counts returned by [`AuthHandshakeStore::sweep_expired`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AuthSweepCounts {
    pub pending_pruned: usize,
    pub device_pruned: usize,
    pub xmpp_pruned: usize,
    pub pending_remaining: usize,
    pub device_remaining: usize,
    pub xmpp_remaining: usize,
}

/// Row counts for the state inventory snapshot.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AuthHandshakeCounts {
    pub pending_auth: usize,
    pub device_auth: usize,
    pub xmpp_auth_codes: usize,
}

#[derive(Clone)]
pub struct AuthHandshakeStore {
    actor: ActorRef<DbActor>,
}

impl AuthHandshakeStore {
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        Self { actor }
    }

    fn ask_err(e: impl std::fmt::Display) -> AuthError {
        AuthError::DatabaseError(e.to_string())
    }

    fn encode<T: Serialize>(entry: &T) -> Result<String, AuthError> {
        serde_json::to_string(entry)
            .map_err(|e| AuthError::DatabaseError(format!("failed to encode payload: {e}")))
    }

    fn decode<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, AuthError> {
        serde_json::from_str(payload)
            .map_err(|e| AuthError::DatabaseError(format!("failed to decode payload: {e}")))
    }

    async fn select_payload(
        &self,
        sql: &'static str,
        key: &str,
    ) -> Result<Option<String>, AuthError> {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: sql.to_string(),
                params: vec![crate::db::Value::from(key)],
            })
            .await
            .map_err(Self::ask_err)?;

        match row {
            Some(values) => {
                let payload = row_value(&values, 0)
                    .and_then(ValueExt::as_string)
                    .map_err(|e| AuthError::DatabaseError(format!("failed to get payload: {e}")))?;
                Ok(Some(payload))
            }
            None => Ok(None),
        }
    }

    /// SELECT the payload, then DELETE gated on rows-affected. Exactly
    /// one concurrent caller observes `rows_affected == 1`; every other
    /// racer gets `None`, which makes redemption single-use across
    /// replicas without needing driver-specific `RETURNING` support.
    async fn take_payload(
        &self,
        select_sql: &'static str,
        delete_sql: &'static str,
        key: &str,
    ) -> Result<Option<String>, AuthError> {
        let Some(payload) = self.select_payload(select_sql, key).await? else {
            return Ok(None);
        };

        let deleted = self
            .actor
            .ask(DbExecute {
                sql: delete_sql.to_string(),
                params: vec![crate::db::Value::from(key)],
            })
            .await
            .map_err(Self::ask_err)?;

        if deleted == 1 {
            Ok(Some(payload))
        } else {
            Ok(None)
        }
    }

    pub async fn insert_pending(&self, pending: &PendingAuthorization) -> Result<(), AuthError> {
        let payload = Self::encode(pending)?;
        self.actor
            .ask(DbExecute {
                sql: "INSERT INTO pending_auth (state, payload, expires_at_ms) VALUES (?, ?, ?)"
                    .to_string(),
                params: vec![
                    crate::db::Value::from(&pending.state),
                    crate::db::Value::from(payload),
                    crate::db::Value::from(pending.expires_at().timestamp_millis()),
                ],
            })
            .await
            .map_err(Self::ask_err)?;
        Ok(())
    }

    pub async fn take_pending(
        &self,
        state: &str,
    ) -> Result<Option<PendingAuthorization>, AuthError> {
        self.take_payload(
            "SELECT payload FROM pending_auth WHERE state = ? LIMIT 1",
            "DELETE FROM pending_auth WHERE state = ?",
            state,
        )
        .await?
        .map(|payload| Self::decode(&payload))
        .transpose()
    }

    pub async fn insert_xmpp_code(
        &self,
        code: &str,
        entry: &XmppAuthCode,
    ) -> Result<(), AuthError> {
        let payload = Self::encode(entry)?;
        self.actor
            .ask(DbExecute {
                sql: "INSERT INTO xmpp_auth_codes (code, payload, expires_at_ms) VALUES (?, ?, ?)"
                    .to_string(),
                params: vec![
                    crate::db::Value::from(code),
                    crate::db::Value::from(payload),
                    crate::db::Value::from(entry.expires_at().timestamp_millis()),
                ],
            })
            .await
            .map_err(Self::ask_err)?;
        Ok(())
    }

    pub async fn take_xmpp_code(&self, code: &str) -> Result<Option<XmppAuthCode>, AuthError> {
        self.take_payload(
            "SELECT payload FROM xmpp_auth_codes WHERE code = ? LIMIT 1",
            "DELETE FROM xmpp_auth_codes WHERE code = ?",
            code,
        )
        .await?
        .map(|payload| Self::decode(&payload))
        .transpose()
    }

    pub async fn insert_device(&self, entry: &DeviceAuthorization) -> Result<(), AuthError> {
        let payload = Self::encode(entry)?;
        self.actor
            .ask(DbExecute {
                sql: "INSERT INTO device_auth (device_code, user_code, payload, expires_at_ms) \
                      VALUES (?, ?, ?, ?)"
                    .to_string(),
                params: vec![
                    crate::db::Value::from(&entry.device_code),
                    crate::db::Value::from(&entry.user_code),
                    crate::db::Value::from(payload),
                    crate::db::Value::from(entry.expires_at.timestamp_millis()),
                ],
            })
            .await
            .map_err(Self::ask_err)?;
        Ok(())
    }

    pub async fn get_device(
        &self,
        device_code: &str,
    ) -> Result<Option<DeviceAuthorization>, AuthError> {
        self.select_payload(
            "SELECT payload FROM device_auth WHERE device_code = ? LIMIT 1",
            device_code,
        )
        .await?
        .map(|payload| Self::decode(&payload))
        .transpose()
    }

    pub async fn find_device_by_user_code(
        &self,
        user_code: &str,
    ) -> Result<Option<DeviceAuthorization>, AuthError> {
        self.select_payload(
            "SELECT payload FROM device_auth WHERE user_code = ? LIMIT 1",
            user_code,
        )
        .await?
        .map(|payload| Self::decode(&payload))
        .transpose()
    }

    /// Persist a device-flow state transition (Pending → InProgress →
    /// Approved / session attach). Returns `false` when the row is gone
    /// (expired-and-pruned or redeemed by another replica).
    pub async fn update_device(&self, entry: &DeviceAuthorization) -> Result<bool, AuthError> {
        let payload = Self::encode(entry)?;
        let updated = self
            .actor
            .ask(DbExecute {
                sql: "UPDATE device_auth SET payload = ?, expires_at_ms = ? WHERE device_code = ?"
                    .to_string(),
                params: vec![
                    crate::db::Value::from(payload),
                    crate::db::Value::from(entry.expires_at.timestamp_millis()),
                    crate::db::Value::from(&entry.device_code),
                ],
            })
            .await
            .map_err(Self::ask_err)?;
        Ok(updated == 1)
    }

    pub async fn remove_device(&self, device_code: &str) -> Result<(), AuthError> {
        self.actor
            .ask(DbExecute {
                sql: "DELETE FROM device_auth WHERE device_code = ?".to_string(),
                params: vec![crate::db::Value::from(device_code)],
            })
            .await
            .map_err(Self::ask_err)?;
        Ok(())
    }

    async fn delete_expired(&self, table: &str, now_ms: i64) -> Result<u64, AuthError> {
        self.actor
            .ask(DbExecute {
                sql: format!("DELETE FROM {table} WHERE expires_at_ms <= ?"),
                params: vec![crate::db::Value::from(now_ms)],
            })
            .await
            .map_err(Self::ask_err)
    }

    async fn count_rows(&self, table: &str) -> Result<usize, AuthError> {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: format!("SELECT COUNT(*) FROM {table}"),
                params: vec![],
            })
            .await
            .map_err(Self::ask_err)?;

        match row {
            Some(values) => {
                let value = row_value(&values, 0).map_err(|e| {
                    AuthError::DatabaseError(format!("failed to get row count: {e}"))
                })?;
                match value {
                    crate::db::Value::Integer(count) => Ok(usize::try_from(*count).unwrap_or(0)),
                    other => Err(AuthError::DatabaseError(format!(
                        "expected integer row count, got {other:?}"
                    ))),
                }
            }
            None => Ok(0),
        }
    }

    /// Delete every entry whose validity window has passed. Used by the
    /// auth-state janitor; any replica may run it since expiry is
    /// derived from the stored `expires_at_ms`, not process state.
    pub async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<AuthSweepCounts, AuthError> {
        let now_ms = now.timestamp_millis();
        let pending_pruned = self.delete_expired("pending_auth", now_ms).await?;
        let device_pruned = self.delete_expired("device_auth", now_ms).await?;
        let xmpp_pruned = self.delete_expired("xmpp_auth_codes", now_ms).await?;
        let counts = self.counts().await?;
        Ok(AuthSweepCounts {
            pending_pruned: usize::try_from(pending_pruned).unwrap_or(usize::MAX),
            device_pruned: usize::try_from(device_pruned).unwrap_or(usize::MAX),
            xmpp_pruned: usize::try_from(xmpp_pruned).unwrap_or(usize::MAX),
            pending_remaining: counts.pending_auth,
            device_remaining: counts.device_auth,
            xmpp_remaining: counts.xmpp_auth_codes,
        })
    }

    pub async fn counts(&self) -> Result<AuthHandshakeCounts, AuthError> {
        Ok(AuthHandshakeCounts {
            pending_auth: self.count_rows("pending_auth").await?,
            device_auth: self.count_rows("device_auth").await?,
            xmpp_auth_codes: self.count_rows("xmpp_auth_codes").await?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{actor::DbActor, Database, MigrationRunner};
    use kameo::actor::Spawn;

    async fn create_store(db_name: &str) -> AuthHandshakeStore {
        let db = Database::in_memory(db_name).await.expect("in-memory db");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        AuthHandshakeStore::new(DbActor::spawn(DbActor::new(db)))
    }

    fn make_pending(state: &str, created_minutes_ago: i64) -> PendingAuthorization {
        PendingAuthorization {
            state: state.to_string(),
            provider_id: "p".to_string(),
            nonce: "n".to_string(),
            code_verifier: "cv".to_string(),
            redirect_uri: "https://example.test/cb".to_string(),
            client_id: "cid".to_string(),
            client_secret: "secret".to_string(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
            require_dpop: false,
            flow: PendingFlow::Browser {
                next: Some("/chat".to_string()),
                session_transport: BrowserSessionTransport::Fragment,
            },
            created_at: Utc::now() - Duration::minutes(created_minutes_ago),
        }
    }

    #[tokio::test]
    async fn pending_round_trips_through_the_store() {
        let store = create_store("handshake-pending-roundtrip").await;
        let pending = make_pending("state-abc", 0);

        store.insert_pending(&pending).await.expect("insert");
        let loaded = store
            .take_pending("state-abc")
            .await
            .expect("take")
            .expect("entry exists");

        assert_eq!(loaded.state, "state-abc");
        assert_eq!(loaded.provider_id, "p");
        assert_eq!(loaded.code_verifier, "cv");
        assert_eq!(loaded.client_secret, "secret");
        assert_eq!(
            loaded.token_endpoint_auth_method,
            AuthProviderTokenEndpointAuthMethod::ClientSecretPost
        );
        match loaded.flow {
            PendingFlow::Browser {
                next,
                session_transport,
            } => {
                assert_eq!(next.as_deref(), Some("/chat"));
                assert_eq!(session_transport, BrowserSessionTransport::Fragment);
            }
            other => panic!("expected browser flow, got {other:?}"),
        }
        assert_eq!(loaded.created_at, pending.created_at);
    }

    #[tokio::test]
    async fn pending_take_is_single_use() {
        let store = create_store("handshake-pending-single-use").await;
        store
            .insert_pending(&make_pending("state-once", 0))
            .await
            .expect("insert");

        assert!(store
            .take_pending("state-once")
            .await
            .expect("first take")
            .is_some());
        assert!(store
            .take_pending("state-once")
            .await
            .expect("second take")
            .is_none());
    }

    #[tokio::test]
    async fn pending_take_of_unknown_state_returns_none() {
        let store = create_store("handshake-pending-unknown").await;
        assert!(store
            .take_pending("never-inserted")
            .await
            .expect("take")
            .is_none());
    }

    fn make_xmpp_code(session_id: &str, created_minutes_ago: i64) -> XmppAuthCode {
        XmppAuthCode {
            session_id: session_id.to_string(),
            redirect_uri: "waddle://auth/callback".to_string(),
            code_challenge: Some("challenge".to_string()),
            created_at: Utc::now() - Duration::minutes(created_minutes_ago),
        }
    }

    #[tokio::test]
    async fn xmpp_code_round_trips_and_is_single_use() {
        let store = create_store("handshake-xmpp-roundtrip").await;
        store
            .insert_xmpp_code("code-1", &make_xmpp_code("session-9", 0))
            .await
            .expect("insert");

        let loaded = store
            .take_xmpp_code("code-1")
            .await
            .expect("take")
            .expect("entry exists");
        assert_eq!(loaded.session_id, "session-9");
        assert_eq!(loaded.redirect_uri, "waddle://auth/callback");
        assert_eq!(loaded.code_challenge.as_deref(), Some("challenge"));

        assert!(store
            .take_xmpp_code("code-1")
            .await
            .expect("second take")
            .is_none());
    }

    fn make_device(code: &str, expires_minutes_from_now: i64) -> DeviceAuthorization {
        DeviceAuthorization {
            device_code: code.to_string(),
            user_code: format!("USER-{code}"),
            provider_id: "p".to_string(),
            expires_at: Utc::now() + Duration::minutes(expires_minutes_from_now),
            status: DeviceAuthStatus::Pending,
            session_id: None,
        }
    }

    #[tokio::test]
    async fn device_state_transitions_persist_across_lookups() {
        let store = create_store("handshake-device-transitions").await;
        let mut device = make_device("dev-1", 15);
        store.insert_device(&device).await.expect("insert");

        let by_user_code = store
            .find_device_by_user_code("USER-dev-1")
            .await
            .expect("find")
            .expect("entry exists");
        assert_eq!(by_user_code.device_code, "dev-1");
        assert_eq!(by_user_code.status, DeviceAuthStatus::Pending);

        device.status = DeviceAuthStatus::Approved;
        device.session_id = Some("session-42".to_string());
        assert!(store.update_device(&device).await.expect("update"));

        let reloaded = store
            .get_device("dev-1")
            .await
            .expect("get")
            .expect("entry exists");
        assert_eq!(reloaded.status, DeviceAuthStatus::Approved);
        assert_eq!(reloaded.session_id.as_deref(), Some("session-42"));

        store.remove_device("dev-1").await.expect("remove");
        assert!(store.get_device("dev-1").await.expect("get").is_none());
        assert!(!store.update_device(&device).await.expect("update gone"));
    }

    #[tokio::test]
    async fn sweep_removes_only_expired_entries() {
        let store = create_store("handshake-sweep").await;

        store
            .insert_pending(&make_pending("fresh", 1))
            .await
            .expect("insert");
        store
            .insert_pending(&make_pending("stale", 30))
            .await
            .expect("insert");
        store
            .insert_device(&make_device("live", 5))
            .await
            .expect("insert");
        store
            .insert_device(&make_device("dead", -1))
            .await
            .expect("insert");
        store
            .insert_xmpp_code("fresh-code", &make_xmpp_code("s", 2))
            .await
            .expect("insert");
        store
            .insert_xmpp_code("stale-code", &make_xmpp_code("s", 20))
            .await
            .expect("insert");

        let counts = store.sweep_expired(Utc::now()).await.expect("sweep");

        assert_eq!(counts.pending_pruned, 1);
        assert_eq!(counts.device_pruned, 1);
        assert_eq!(counts.xmpp_pruned, 1);
        assert_eq!(counts.pending_remaining, 1);
        assert_eq!(counts.device_remaining, 1);
        assert_eq!(counts.xmpp_remaining, 1);

        assert!(store.take_pending("fresh").await.expect("take").is_some());
        assert!(store.take_pending("stale").await.expect("take").is_none());
        assert!(store.get_device("live").await.expect("get").is_some());
        assert!(store.get_device("dead").await.expect("get").is_none());
        assert!(store
            .take_xmpp_code("fresh-code")
            .await
            .expect("take")
            .is_some());
        assert!(store
            .take_xmpp_code("stale-code")
            .await
            .expect("take")
            .is_none());
    }

    #[tokio::test]
    async fn two_stores_sharing_one_database_behave_like_two_pods() {
        // Two `AuthHandshakeStore`s over the same database simulate two
        // replicas behind the load balancer: pod A mints the state on
        // /api/auth/start, pod B redeems it on /api/auth/callback.
        let db = Database::in_memory("handshake-two-pods")
            .await
            .expect("in-memory db");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        let pod_a = AuthHandshakeStore::new(DbActor::spawn(DbActor::new(db.clone())));
        let pod_b = AuthHandshakeStore::new(DbActor::spawn(DbActor::new(db)));

        pod_a
            .insert_pending(&make_pending("cross-pod-state", 0))
            .await
            .expect("insert on pod A");

        let redeemed = pod_b
            .take_pending("cross-pod-state")
            .await
            .expect("take on pod B")
            .expect("pod B sees the state pod A minted");
        assert_eq!(redeemed.code_verifier, "cv");

        // Single-use holds across pods too: pod A can no longer redeem.
        assert!(pod_a
            .take_pending("cross-pod-state")
            .await
            .expect("re-take on pod A")
            .is_none());
    }
}
