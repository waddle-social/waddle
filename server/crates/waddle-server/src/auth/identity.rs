use crate::auth::{username_to_localpart, AuthError, AuthProviderConfig};
use crate::db::Database;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityClaims {
    pub subject: String,
    pub issuer: Option<String>,
    pub preferred_username: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub avatar_url: Option<String>,
    pub raw_claims: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: String,
    pub username: String,
    pub xmpp_localpart: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub primary_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedIdentity {
    pub provider_id: String,
    pub subject: String,
    pub user: UserRecord,
}

pub struct IdentityService {
    db: Arc<Database>,
}

impl IdentityService {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub async fn resolve_or_create_user(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
    ) -> Result<LinkedIdentity, AuthError> {
        if claims.subject.trim().is_empty() {
            return Err(AuthError::InvalidRequest(
                "missing provider subject claim".to_string(),
            ));
        }

        if let Some(existing) = self
            .find_by_provider_subject(&provider.id, &claims.subject)
            .await?
        {
            self.update_identity_last_login(provider, claims).await?;
            return Ok(LinkedIdentity {
                provider_id: provider.id.clone(),
                subject: claims.subject.clone(),
                user: existing,
            });
        }

        let user = self.create_user(provider, claims).await?;
        self.insert_identity(provider, claims, &user.id).await?;

        Ok(LinkedIdentity {
            provider_id: provider.id.clone(),
            subject: claims.subject.clone(),
            user,
        })
    }

    async fn find_by_provider_subject(
        &self,
        provider_id: &str,
        subject: &str,
    ) -> Result<Option<UserRecord>, AuthError> {
        let query = r#"
            SELECT u.id, u.username, u.xmpp_localpart, u.display_name, u.avatar_url, u.primary_email
            FROM auth_identities ai
            JOIN users u ON u.id = ai.user_id
            WHERE ai.provider_id = ? AND ai.subject = ?
            LIMIT 1
        "#;

        let row = if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            let mut rows = conn
                .query(query, libsql::params![provider_id, subject])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to query identity: {}", e))
                })?;
            rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read identity row: {}", e))
            })?
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            let mut rows = conn
                .query(query, libsql::params![provider_id, subject])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to query identity: {}", e))
                })?;
            rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read identity row: {}", e))
            })?
        };

        match row {
            Some(row) => Ok(Some(UserRecord {
                id: row.get(0).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get user id: {}", e))
                })?,
                username: row.get(1).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get username: {}", e))
                })?,
                xmpp_localpart: row.get(2).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get xmpp_localpart: {}", e))
                })?,
                display_name: row.get::<Option<String>>(3).ok().flatten(),
                avatar_url: row.get::<Option<String>>(4).ok().flatten(),
                primary_email: row.get::<Option<String>>(5).ok().flatten(),
            })),
            None => Ok(None),
        }
    }

    fn derive_base_username(provider: &AuthProviderConfig, claims: &IdentityClaims) -> String {
        if let Some(v) = claims.preferred_username.as_deref() {
            let slug = username_to_localpart(v);
            if !slug.is_empty() {
                return slug;
            }
        }

        if let Some(claim_key) = provider.username_claim.as_deref() {
            if let Some(v) = claims.raw_claims.get(claim_key).and_then(|v| v.as_str()) {
                let slug = username_to_localpart(v);
                if !slug.is_empty() {
                    return slug;
                }
            }
        }

        if let Some(email) = claims.email.as_deref() {
            if let Some((local, _)) = email.split_once('@') {
                let slug = username_to_localpart(local);
                if !slug.is_empty() {
                    return slug;
                }
            }
        }

        let provider_slug = username_to_localpart(&provider.id);
        let provider_component = if provider_slug.is_empty() {
            "provider".to_string()
        } else {
            provider_slug
        };

        let digest = Sha256::digest(format!("{}:{}", provider.id, claims.subject).as_bytes());
        let short = hex::encode(&digest[..6]);
        format!("ext_{}_{}", provider_component, short)
    }

    async fn create_user(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
    ) -> Result<UserRecord, AuthError> {
        let base = Self::derive_base_username(provider, claims);

        for i in 0..200 {
            let username = if i == 0 {
                base.clone()
            } else {
                format!("{}{}", base, i)
            };
            let xmpp_localpart = username_to_localpart(&username);
            let user_id = Uuid::new_v4().to_string();
            let now = Utc::now().to_rfc3339();

            let insert = r#"
                INSERT INTO users (
                    id, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#;

            let result = if let Some(persistent) = self.db.persistent_connection() {
                let conn = persistent.lock().await;
                conn.execute(
                    insert,
                    libsql::params![
                        user_id.clone(),
                        username.clone(),
                        xmpp_localpart.clone(),
                        claims.name.clone(),
                        claims.avatar_url.clone(),
                        claims.email.clone(),
                        now.clone(),
                        now.clone()
                    ],
                )
                .await
            } else {
                let conn = self.db.connect().map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to connect database: {}", e))
                })?;
                conn.execute(
                    insert,
                    libsql::params![
                        user_id.clone(),
                        username.clone(),
                        xmpp_localpart.clone(),
                        claims.name.clone(),
                        claims.avatar_url.clone(),
                        claims.email.clone(),
                        now.clone(),
                        now.clone()
                    ],
                )
                .await
            };

            match result {
                Ok(_) => {
                    return Ok(UserRecord {
                        id: user_id,
                        username,
                        xmpp_localpart,
                        display_name: claims.name.clone(),
                        avatar_url: claims.avatar_url.clone(),
                        primary_email: claims.email.clone(),
                    });
                }
                Err(err) => {
                    let msg = err.to_string();
                    if msg.contains("UNIQUE") || msg.contains("constraint") {
                        continue;
                    }
                    return Err(AuthError::DatabaseError(format!(
                        "Failed to insert user: {}",
                        err
                    )));
                }
            }
        }

        Err(AuthError::DatabaseError(
            "Failed to allocate unique username".to_string(),
        ))
    }

    async fn insert_identity(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
        user_id: &str,
    ) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();
        let identity_id = Uuid::new_v4().to_string();
        let raw = serde_json::to_string(&claims.raw_claims)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to serialize claims: {}", e)))?;

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                r#"
                INSERT INTO auth_identities (
                    id, user_id, provider_id, issuer, subject, email, email_verified,
                    raw_claims_json, created_at, last_login_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                libsql::params![
                    identity_id.as_str(),
                    user_id,
                    provider.id.as_str(),
                    claims.issuer.clone(),
                    claims.subject.as_str(),
                    claims.email.clone(),
                    claims.email_verified.map(|v| if v { 1 } else { 0 }),
                    raw.as_str(),
                    now.as_str(),
                    now.as_str()
                ],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert identity: {}", e)))?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            conn.execute(
                r#"
                INSERT INTO auth_identities (
                    id, user_id, provider_id, issuer, subject, email, email_verified,
                    raw_claims_json, created_at, last_login_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                libsql::params![
                    identity_id.as_str(),
                    user_id,
                    provider.id.as_str(),
                    claims.issuer.clone(),
                    claims.subject.as_str(),
                    claims.email.clone(),
                    claims.email_verified.map(|v| if v { 1 } else { 0 }),
                    raw.as_str(),
                    now.as_str(),
                    now.as_str()
                ],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert identity: {}", e)))?;
        }

        Ok(())
    }

    async fn update_identity_last_login(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
    ) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                "UPDATE auth_identities SET last_login_at = ? WHERE provider_id = ? AND subject = ?",
                libsql::params![now.as_str(), provider.id.as_str(), claims.subject.as_str()],
            )
            .await
            .map_err(|e| {
                AuthError::DatabaseError(format!("Failed to update identity login timestamp: {}", e))
            })?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            conn.execute(
                "UPDATE auth_identities SET last_login_at = ? WHERE provider_id = ? AND subject = ?",
                libsql::params![now.as_str(), provider.id.as_str(), claims.subject.as_str()],
            )
            .await
            .map_err(|e| {
                AuthError::DatabaseError(format!("Failed to update identity login timestamp: {}", e))
            })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthProviderKind, AuthProviderTokenEndpointAuthMethod};
    use serde_json::json;

    fn provider_with_username_claim(claim: Option<&str>) -> AuthProviderConfig {
        AuthProviderConfig {
            id: "provider".to_string(),
            display_name: "Provider".to_string(),
            kind: AuthProviderKind::Oidc,
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ],
            issuer: Some("https://issuer.example".to_string()),
            authorization_endpoint: None,
            token_endpoint: None,
            userinfo_endpoint: None,
            jwks_uri: None,
            subject_claim: "sub".to_string(),
            username_claim: claim.map(|v| v.to_string()),
            email_claim: Some("email".to_string()),
        }
    }

    fn claims() -> IdentityClaims {
        IdentityClaims {
            subject: "sub-1234".to_string(),
            issuer: Some("https://issuer.example".to_string()),
            preferred_username: None,
            name: Some("Example".to_string()),
            email: Some("example.user@waddle.test".to_string()),
            email_verified: Some(true),
            avatar_url: None,
            raw_claims: json!({}),
        }
    }

    #[test]
    fn username_prefers_preferred_username_claim() {
        let provider = provider_with_username_claim(Some("login"));
        let mut claims = claims();
        claims.preferred_username = Some("Alice.Dev".to_string());
        claims.raw_claims = json!({ "login": "ignored-login" });

        let username = IdentityService::derive_base_username(&provider, &claims);
        assert_eq!(username, "alice.dev");
    }

    #[test]
    fn username_uses_provider_specific_claim_before_email() {
        let provider = provider_with_username_claim(Some("login"));
        let mut claims = claims();
        claims.raw_claims = json!({ "login": "octo-cat" });

        let username = IdentityService::derive_base_username(&provider, &claims);
        assert_eq!(username, "octo-cat");
    }

    #[test]
    fn username_falls_back_to_email_local_part() {
        let provider = provider_with_username_claim(Some("login"));
        let claims = claims();

        let username = IdentityService::derive_base_username(&provider, &claims);
        assert_eq!(username, "example.user");
    }

    #[test]
    fn username_falls_back_to_provider_hash_when_needed() {
        let provider = provider_with_username_claim(None);
        let mut claims = claims();
        claims.preferred_username = None;
        claims.email = None;
        claims.raw_claims = json!({});

        let username = IdentityService::derive_base_username(&provider, &claims);
        let prefix = "ext_provider_";
        assert!(username.starts_with(prefix));
        let suffix = &username[prefix.len()..];
        assert_eq!(suffix.len(), 12);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
