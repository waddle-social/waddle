use crate::auth::{localpart_to_jid, username_to_localpart, AuthError, AuthProviderConfig};
use kameo::actor::ActorRef;

use crate::db::actor::{DbActor, DbExecute, DbQueryOne};
use crate::db::{row_value, ValueExt};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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
    /// Bare JID principal (e.g. `alice@example.com`). Immutable once created.
    pub jid: String,
    pub username: String,
    pub xmpp_localpart: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub primary_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedIdentity {
    pub issuer: String,
    pub subject: String,
    pub user: UserRecord,
}

pub struct IdentityService {
    actor: ActorRef<DbActor>,
}

impl IdentityService {
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        Self { actor }
    }

    pub async fn resolve_or_create_user(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
        domain: &str,
    ) -> Result<LinkedIdentity, AuthError> {
        let subject = claims.subject.trim();
        if subject.is_empty() {
            return Err(AuthError::InvalidRequest(
                "missing provider subject claim".to_string(),
            ));
        }

        let issuer = Self::identity_issuer(provider, claims)?;

        if let Some(existing) = self.find_by_issuer_subject(&issuer, subject).await? {
            // The bare JID is immutable: reconciliation only refreshes
            // mutable profile fields, never the localpart/JID/username.
            let user = self.reconcile_existing_user(claims, &existing).await?;
            self.update_identity_last_login(provider, claims, &issuer, subject)
                .await?;
            return Ok(LinkedIdentity {
                issuer,
                subject: subject.to_string(),
                user,
            });
        }

        let user = self.create_user(provider, claims, &issuer, domain).await?;
        self.insert_identity(provider, claims, &issuer, &user.jid)
            .await?;

        Ok(LinkedIdentity {
            issuer,
            subject: subject.to_string(),
            user,
        })
    }

    fn identity_issuer(
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
    ) -> Result<String, AuthError> {
        claims
            .issuer
            .as_deref()
            .or(provider.issuer.as_deref())
            .map(Self::normalize_issuer)
            .filter(|issuer| !issuer.is_empty())
            .ok_or_else(|| {
                AuthError::InvalidRequest(
                    "missing issuer for identity mapping (issuer+sub required)".to_string(),
                )
            })
    }

    fn normalize_issuer(issuer: &str) -> String {
        issuer.trim().trim_end_matches('/').to_string()
    }

    async fn find_by_issuer_subject(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<UserRecord>, AuthError> {
        let query = r#"
            SELECT u.jid, u.username, u.xmpp_localpart, u.display_name, u.avatar_url, u.primary_email
            FROM auth_identities ai
            JOIN users u ON u.jid = ai.user_jid
            WHERE ai.issuer = ? AND ai.subject = ?
            LIMIT 1
        "#;

        let row = self
            .actor
            .ask(DbQueryOne {
                sql: query.to_string(),
                params: vec![issuer.into(), subject.into()],
            })
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to query identity: {}", e)))?;

        match row {
            Some(row) => Ok(Some(UserRecord {
                jid: row_value(&row, 0)
                    .and_then(ValueExt::as_string)
                    .map_err(|e| {
                        AuthError::DatabaseError(format!("Failed to get user jid: {}", e))
                    })?,
                username: row_value(&row, 1)
                    .and_then(ValueExt::as_string)
                    .map_err(|e| {
                        AuthError::DatabaseError(format!("Failed to get username: {}", e))
                    })?,
                xmpp_localpart: row_value(&row, 2)
                    .and_then(ValueExt::as_string)
                    .map_err(|e| {
                        AuthError::DatabaseError(format!("Failed to get xmpp_localpart: {}", e))
                    })?,
                display_name: row_value(&row, 3)
                    .and_then(ValueExt::as_optional_string)
                    .map_err(|e| {
                        AuthError::DatabaseError(format!("Failed to get display_name: {}", e))
                    })?,
                avatar_url: row_value(&row, 4)
                    .and_then(ValueExt::as_optional_string)
                    .map_err(|e| {
                        AuthError::DatabaseError(format!("Failed to get avatar_url: {}", e))
                    })?,
                primary_email: row_value(&row, 5)
                    .and_then(ValueExt::as_optional_string)
                    .map_err(|e| {
                        AuthError::DatabaseError(format!("Failed to get primary_email: {}", e))
                    })?,
            })),
            None => Ok(None),
        }
    }

    fn derive_base_username(
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
        issuer: &str,
    ) -> String {
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

        let digest = Sha256::digest(format!("{}:{}", issuer, claims.subject.trim()).as_bytes());
        let short = hex::encode(&digest[..6]);
        format!("ext_{}_{}", provider_component, short)
    }

    async fn create_user(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
        issuer: &str,
        domain: &str,
    ) -> Result<UserRecord, AuthError> {
        let base = Self::derive_base_username(provider, claims, issuer);

        for i in 0..200 {
            let username = if i == 0 {
                base.clone()
            } else {
                format!("{}{}", base, i)
            };
            let xmpp_localpart = username_to_localpart(&username);
            // The bare JID is the immutable principal and primary key.
            let jid = localpart_to_jid(&xmpp_localpart, domain)?;
            let now = Utc::now().to_rfc3339();

            let insert = r#"
                INSERT INTO users (
                    jid, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#;

            let result = self
                .actor
                .ask(DbExecute {
                    sql: insert.to_string(),
                    params: vec![
                        jid.clone().into(),
                        username.clone().into(),
                        xmpp_localpart.clone().into(),
                        claims.name.clone().into(),
                        claims.avatar_url.clone().into(),
                        claims.email.clone().into(),
                        now.clone().into(),
                        now.clone().into(),
                    ],
                })
                .await;

            match result {
                Ok(_) => {
                    return Ok(UserRecord {
                        jid,
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

    /// Refresh the mutable profile fields of an existing user from the
    /// latest identity claims.
    ///
    /// The bare JID is the immutable principal (primary key referenced by
    /// sessions, identities, messages, and permission tuples), so the
    /// localpart/JID/username are never rewritten here — only
    /// `display_name`, `avatar_url`, and `primary_email`.
    async fn reconcile_existing_user(
        &self,
        claims: &IdentityClaims,
        existing: &UserRecord,
    ) -> Result<UserRecord, AuthError> {
        let now = Utc::now().to_rfc3339();

        self.actor
            .ask(DbExecute {
                sql: r#"
                    UPDATE users
                    SET display_name = ?, avatar_url = ?, primary_email = ?, updated_at = ?
                    WHERE jid = ?
                "#
                .to_string(),
                params: vec![
                    claims.name.clone().into(),
                    claims.avatar_url.clone().into(),
                    claims.email.clone().into(),
                    now.into(),
                    existing.jid.clone().into(),
                ],
            })
            .await
            .map_err(|err| {
                AuthError::DatabaseError(format!(
                    "Failed to update user from identity claims: {}",
                    err
                ))
            })?;

        Ok(UserRecord {
            jid: existing.jid.clone(),
            username: existing.username.clone(),
            xmpp_localpart: existing.xmpp_localpart.clone(),
            display_name: claims.name.clone(),
            avatar_url: claims.avatar_url.clone(),
            primary_email: claims.email.clone(),
        })
    }

    async fn insert_identity(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
        issuer: &str,
        user_jid: &str,
    ) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();
        let identity_id = Uuid::new_v4().to_string();
        let raw = serde_json::to_string(&claims.raw_claims)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to serialize claims: {}", e)))?;

        self.actor
            .ask(DbExecute {
                sql: r#"
                    INSERT INTO auth_identities (
                        id, user_jid, provider_id, issuer, subject, email, email_verified,
                        raw_claims_json, created_at, last_login_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#
                .to_string(),
                params: vec![
                    identity_id.into(),
                    user_jid.into(),
                    provider.id.clone().into(),
                    issuer.into(),
                    claims.subject.clone().into(),
                    claims.email.clone().into(),
                    // `email_verified` is INTEGER. Map through i64 so
                    // the typed-null path picks `NullInteger` for None
                    // (Postgres binds `Option::<i64>::None`); a bare
                    // `Option<bool>::into()` would also produce
                    // `NullInteger`, but the explicit `i64::from` keeps
                    // the Some-arm wire shape obvious next to the email
                    // text bind above.
                    claims.email_verified.map(i64::from).into(),
                    raw.into(),
                    now.clone().into(),
                    now.into(),
                ],
            })
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert identity: {}", e)))?;

        Ok(())
    }

    async fn update_identity_last_login(
        &self,
        provider: &AuthProviderConfig,
        claims: &IdentityClaims,
        issuer: &str,
        subject: &str,
    ) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();
        let raw = serde_json::to_string(&claims.raw_claims)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to serialize claims: {}", e)))?;

        self.actor
            .ask(DbExecute {
                sql: "UPDATE auth_identities
                      SET last_login_at = ?, provider_id = ?, email = ?, email_verified = ?, raw_claims_json = ?
                      WHERE issuer = ? AND subject = ?"
                    .to_string(),
                params: vec![
                    now.into(),
                    provider.id.clone().into(),
                    claims.email.clone().into(),
                    // INTEGER column — see comment on the insert path above.
                    claims.email_verified.map(i64::from).into(),
                    raw.into(),
                    issuer.into(),
                    subject.into(),
                ],
            })
            .await
            .map_err(|e| {
                AuthError::DatabaseError(format!("Failed to update identity login timestamp: {}", e))
            })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests;
