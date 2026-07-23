use crate::auth::{NativeUserStore, RegisterRequest};
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

fn fixed_test_account_seeding_enabled() -> bool {
    std::env::var("WADDLE_TEST_FIXED_ACCOUNT_SEED")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true)
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
    // Multi-process integration tests may provision the shared accounts once
    // before concurrently starting several servers. Keep the fixed-account
    // permission backend enabled while skipping its deliberately destructive
    // delete-and-recreate seeding on those concurrent starts.
    if !fixed_test_account_seeding_enabled() {
        info!("Skipping fixed native test-account seeding");
        return Ok(());
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

    info!(
        username = %config.username,
        domain = %config.domain,
        "Provisioned fixed native test account"
    );
    Ok(())
}
