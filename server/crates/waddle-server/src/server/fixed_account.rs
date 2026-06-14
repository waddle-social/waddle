use crate::auth::{NativeUserStore, RegisterRequest};
use crate::db::actor::{DbExecute, DbQueryOne};
use crate::db::DatabasePool;
use crate::server::XmppConfig;
use anyhow::Result;
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone)]
pub(crate) struct FixedTestAccountConfig {
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) domain: String,
    pub(crate) email: Option<String>,
}

pub(crate) fn fixed_test_account_enabled() -> bool {
    std::env::var("WADDLE_TEST_FIXED_ACCOUNT_ENABLED")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

pub(crate) async fn ensure_fixed_test_account(
    db_pool: &Arc<DatabasePool>,
    xmpp_config: &XmppConfig,
) -> Result<()> {
    let enabled = fixed_test_account_enabled();
    if !enabled {
        return Ok(());
    }

    if !xmpp_config.enabled {
        anyhow::bail!("WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true requires WADDLE_XMPP_ENABLED=true");
    }
    if !xmpp_config.native_auth_enabled {
        anyhow::bail!(
            "WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true requires WADDLE_NATIVE_AUTH_ENABLED=true"
        );
    }

    let username = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_USERNAME")
        .unwrap_or_else(|_| "admin".to_string())
        .trim()
        .to_string();
    if username.is_empty() {
        anyhow::bail!("WADDLE_TEST_FIXED_ACCOUNT_USERNAME cannot be empty");
    }

    let password = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_PASSWORD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD must be set when WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true"
            )
        })?;

    let domain = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_DOMAIN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| xmpp_config.domain.clone());
    let email = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_EMAIL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    seed_fixed_test_account(
        db_pool,
        &FixedTestAccountConfig {
            username,
            password,
            domain: domain.clone(),
            email,
        },
    )
    .await?;

    if let Ok(extra_accounts) = std::env::var("WADDLE_TEST_EXTRA_FIXED_ACCOUNTS") {
        for account in extra_accounts
            .split(',')
            .filter(|entry| !entry.trim().is_empty())
        {
            let Some((username, password)) = account.split_once(':') else {
                anyhow::bail!("WADDLE_TEST_EXTRA_FIXED_ACCOUNTS entries must be username:password");
            };
            seed_fixed_test_account(
                db_pool,
                &FixedTestAccountConfig {
                    username: username.trim().to_string(),
                    password: password.trim().to_string(),
                    domain: domain.clone(),
                    email: None,
                },
            )
            .await?;
        }
    }

    Ok(())
}

pub(crate) async fn seed_fixed_test_account(
    db_pool: &Arc<DatabasePool>,
    config: &FixedTestAccountConfig,
) -> Result<()> {
    let native_user_store = NativeUserStore::new(db_pool.global_actor().clone());
    if native_user_store
        .user_exists(&config.username, &config.domain)
        .await
        .map_err(|err| anyhow::anyhow!("Failed checking fixed test account: {err}"))?
    {
        native_user_store
            .delete_user(&config.username, &config.domain)
            .await
            .map_err(|err| anyhow::anyhow!("Failed resetting fixed test account: {err}"))?;
    }

    native_user_store
        .register(RegisterRequest {
            username: config.username.clone(),
            domain: config.domain.clone(),
            password: config.password.clone(),
            email: config.email.clone(),
        })
        .await
        .map_err(|err| anyhow::anyhow!("Failed creating fixed test account: {err}"))?;

    // Provision a matching OIDC `users` identity so the account has a
    // `users.id` — the canonical permission subject. Real deployments only have
    // OIDC users; native-only test accounts cannot be group-DM members or carry
    // any SpiceDB affiliation, so the fixed-account harness mirrors a real
    // identity here.
    ensure_oidc_identity_row(db_pool, config).await?;

    info!(
        username = %config.username,
        domain = %config.domain,
        "Provisioned fixed native test account"
    );
    Ok(())
}

/// Upsert an OIDC `users` row for a fixed test account (idempotent across
/// server restarts on a persistent DB). `display_name` is left NULL so the
/// admin Users panel's `users`∪`native_users` walk still dedupes by localpart.
async fn ensure_oidc_identity_row(
    db_pool: &Arc<DatabasePool>,
    config: &FixedTestAccountConfig,
) -> Result<()> {
    // Normalize the localpart the same way the OIDC path and the JID node
    // resolution do, so SCRAM-session and group-DM membership lookups (which key
    // on the nodeprep-normalized localpart) resolve to this row.
    let xmpp_localpart = crate::auth::username_to_localpart(&config.username);
    let actor = db_pool.global_actor();
    let existing = actor
        .ask(DbQueryOne {
            sql: "SELECT 1 FROM users WHERE xmpp_localpart = ? LIMIT 1".to_string(),
            params: vec![xmpp_localpart.as_str().into()],
        })
        .await
        .map_err(|err| anyhow::anyhow!("Failed checking fixed OIDC identity: {err}"))?;
    if existing.is_some() {
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339();
    actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO users
                    (id, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at)
                VALUES (?, ?, ?, NULL, NULL, NULL, ?, ?)
            "#
            .to_string(),
            params: vec![
                uuid::Uuid::new_v4().to_string().into(),
                config.username.as_str().into(),
                xmpp_localpart.as_str().into(),
                now.clone().into(),
                now.into(),
            ],
        })
        .await
        .map_err(|err| anyhow::anyhow!("Failed creating fixed OIDC identity: {err}"))?;
    Ok(())
}
