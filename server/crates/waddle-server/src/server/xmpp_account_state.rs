use kameo::actor::ActorRef;
use tracing::{debug, warn};
use waddle_xmpp::{UserDirectoryEntry, XmppError};

use crate::auth::{localpart_to_jid, AuthError, NativeUserStore, RegisterRequest};
use crate::db::actor::{DbActor, DbQuery};
use crate::db::{row_value, ValueExt};
use crate::permissions::PermissionActor;
use crate::server::bootstrap_membership::{provision_user_membership, BootstrapMembershipConfig};

pub(crate) async fn search_users(
    global_db_actor: &ActorRef<DbActor>,
    domain: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<UserDirectoryEntry>, XmppError> {
    let pattern = format!("%{}%", query.trim());
    let rows = global_db_actor
        .ask(DbQuery {
            sql: r#"
                    SELECT username, xmpp_localpart, display_name, avatar_url
                    FROM users
                    WHERE username LIKE ? OR xmpp_localpart LIKE ? OR display_name LIKE ?
                    ORDER BY username ASC
                    LIMIT ?
                "#
            .to_string(),
            params: vec![
                pattern.as_str().into(),
                pattern.as_str().into(),
                pattern.as_str().into(),
                (limit as i64).into(),
            ],
        })
        .await
        .map_err(|e| XmppError::internal(format!("Failed to search users: {}", e)))?;

    rows.iter()
        .map(|row| {
            let username = db_string(row, 0, "username")
                .map_err(|e| XmppError::internal(format!("Failed to decode username: {}", e)))?;
            let localpart = db_string(row, 1, "xmpp_localpart").map_err(|e| {
                XmppError::internal(format!("Failed to decode xmpp_localpart: {}", e))
            })?;
            let display_name = db_optional_string(row, 2, "display_name").map_err(|e| {
                XmppError::internal(format!("Failed to decode display_name: {}", e))
            })?;
            let avatar_url = db_optional_string(row, 3, "avatar_url")
                .map_err(|e| XmppError::internal(format!("Failed to decode avatar_url: {}", e)))?;
            let jid = localpart_to_jid(&localpart, domain)
                .map_err(|e| XmppError::internal(format!("Failed to build user JID: {}", e)))?
                .parse()
                .map_err(|e| XmppError::internal(format!("Failed to parse user JID: {}", e)))?;
            Ok(UserDirectoryEntry {
                jid,
                username,
                display_name,
                avatar_url,
            })
        })
        .collect()
}

pub(crate) async fn lookup_scram_credentials(
    native_user_store: &NativeUserStore,
    domain: &str,
    username: &str,
) -> Result<Option<waddle_xmpp::ScramCredentials>, XmppError> {
    debug!(
        username = username,
        domain = %domain,
        "Looking up SCRAM credentials for native user"
    );

    match native_user_store
        .get_scram_credentials(username, domain)
        .await
    {
        Ok(Some(creds)) => {
            debug!(username = username, "Found SCRAM credentials");
            Ok(Some(creds))
        }
        Ok(None) => {
            debug!(username = username, "No SCRAM credentials found");
            Ok(None)
        }
        Err(e) => {
            warn!(username = username, error = %e, "Failed to lookup SCRAM credentials");
            Err(XmppError::internal(format!("Database error: {}", e)))
        }
    }
}

pub(crate) async fn register_native_user(
    native_user_store: &NativeUserStore,
    permission_actor: &ActorRef<PermissionActor>,
    domain: &str,
    username: &str,
    password: &str,
    email: Option<&str>,
) -> Result<(), XmppError> {
    debug!(
        username = username,
        domain = %domain,
        has_email = email.is_some(),
        "Registering native user via XEP-0077"
    );

    let request = RegisterRequest {
        username: username.to_string(),
        domain: domain.to_string(),
        password: password.to_string(),
        email: email.map(|s| s.to_string()),
    };

    match native_user_store.register(request).await {
        Ok(user_id) => {
            let subject_user_id = format!("{}@{}", username, domain);
            provision_user_membership(
                permission_actor,
                &BootstrapMembershipConfig::from_env(),
                &subject_user_id,
                username,
            )
            .await
            .map_err(|err| {
                XmppError::internal(format!("Failed to provision account membership: {err}"))
            })?;
            debug!(
                username = username,
                user_id = user_id,
                "Native user registered successfully"
            );
            Ok(())
        }
        Err(AuthError::UserAlreadyExists(_)) => {
            warn!(
                username = username,
                "Registration failed: user already exists"
            );
            Err(XmppError::conflict(Some(format!(
                "User '{}' already exists",
                username
            ))))
        }
        Err(AuthError::InvalidUsername(msg)) => {
            warn!(username = username, error = %msg, "Registration failed: invalid username");
            Err(XmppError::not_acceptable(Some(msg)))
        }
        Err(e) => {
            warn!(username = username, error = %e, "Registration failed");
            Err(XmppError::internal(format!("Registration failed: {}", e)))
        }
    }
}

pub(crate) async fn native_user_exists(
    native_user_store: &NativeUserStore,
    domain: &str,
    username: &str,
) -> Result<bool, XmppError> {
    debug!(
        username = username,
        domain = %domain,
        "Checking if native user exists"
    );

    match native_user_store.user_exists(username, domain).await {
        Ok(exists) => Ok(exists),
        Err(e) => {
            warn!(username = username, error = %e, "Failed to check user existence");
            Err(XmppError::internal(format!("Database error: {}", e)))
        }
    }
}

fn db_string(
    row: &crate::db::actor::RowValues,
    index: usize,
    name: &str,
) -> Result<String, String> {
    row_value(row, index)
        .and_then(ValueExt::as_string)
        .map_err(|e| format!("Failed to get {name}: {e}"))
}

fn db_optional_string(
    row: &crate::db::actor::RowValues,
    index: usize,
    name: &str,
) -> Result<Option<String>, String> {
    row_value(row, index)
        .and_then(ValueExt::as_optional_string)
        .map_err(|e| format!("Failed to get {name}: {e}"))
}
