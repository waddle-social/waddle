use super::*;
use crate::auth::{AuthProviderKind, AuthProviderTokenEndpointAuthMethod};
use crate::db::{Database, MigrationRunner};
use kameo::actor::Spawn;
use serde_json::json;
use std::sync::Arc;

fn provider_with_username_claim(claim: Option<&str>) -> AuthProviderConfig {
    AuthProviderConfig {
        id: "provider".to_string(),
        display_name: "Provider".to_string(),
        kind: AuthProviderKind::Oidc,
        dynamic_client_registration: false,
        client_id: "client".to_string(),
        client_secret: "secret".to_string(),
        token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
        require_dpop: false,
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

async fn create_test_db() -> Arc<Database> {
    let db = Database::in_memory("test-identity-service")
        .await
        .expect("failed to create test database");
    let db = Arc::new(db);
    let runner = MigrationRunner::global();
    runner.run(&db).await.expect("failed to run migrations");
    db
}

#[test]
fn username_prefers_preferred_username_claim() {
    let provider = provider_with_username_claim(Some("login"));
    let mut claims = claims();
    claims.preferred_username = Some("Alice.Dev".to_string());
    claims.raw_claims = json!({ "login": "ignored-login" });

    let username = IdentityService::derive_base_username(
        &provider,
        &claims,
        claims.issuer.as_deref().unwrap(),
    );
    assert_eq!(username, "alice.dev");
}

#[test]
fn username_uses_provider_specific_claim_before_email() {
    let provider = provider_with_username_claim(Some("login"));
    let mut claims = claims();
    claims.raw_claims = json!({ "login": "octo-cat" });

    let username = IdentityService::derive_base_username(
        &provider,
        &claims,
        claims.issuer.as_deref().unwrap(),
    );
    assert_eq!(username, "octo-cat");
}

#[test]
fn username_falls_back_to_email_local_part() {
    let provider = provider_with_username_claim(Some("login"));
    let claims = claims();

    let username = IdentityService::derive_base_username(
        &provider,
        &claims,
        claims.issuer.as_deref().unwrap(),
    );
    assert_eq!(username, "example.user");
}

#[test]
fn username_falls_back_to_provider_hash_when_needed() {
    let provider = provider_with_username_claim(None);
    let mut claims = claims();
    claims.preferred_username = None;
    claims.email = None;
    claims.raw_claims = json!({});

    let username = IdentityService::derive_base_username(
        &provider,
        &claims,
        claims.issuer.as_deref().unwrap(),
    );
    let prefix = "ext_provider_";
    assert!(username.starts_with(prefix));
    let suffix = &username[prefix.len()..];
    assert_eq!(suffix.len(), 12);
    assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn issuer_is_normalized_for_identity_mapping() {
    let provider = provider_with_username_claim(None);
    let mut claims = claims();
    claims.issuer = Some("https://issuer.example/".to_string());

    let issuer = IdentityService::identity_issuer(&provider, &claims).expect("issuer");
    assert_eq!(issuer, "https://issuer.example");
}

#[tokio::test]
async fn existing_identity_keeps_immutable_jid_and_updates_profile_on_login() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let service = IdentityService::new(actor);
    let provider = provider_with_username_claim(Some("preferred_username"));

    {
        let conn = db.guard().await.expect("db guard");
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO users (jid, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "example.person@waddle.test",
                "example.person",
                "example.person",
                "Example Person",
                Option::<String>::None,
                "example.person@waddle.test",
                now.as_str(),
                now.as_str()
            ],
        )
        .await
        .expect("insert user");

        conn.execute(
            r#"
            INSERT INTO auth_identities (
                id, user_jid, provider_id, issuer, subject, email, email_verified,
                raw_claims_json, created_at, last_login_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "identity-1",
                "example.person@waddle.test",
                "provider",
                "https://issuer.example",
                "sub-1234",
                "example.person@waddle.test",
                1,
                "{}",
                now.as_str(),
                now.as_str()
            ],
        )
        .await
        .expect("insert identity");
    }

    // The IDP now reports a different preferred username, display name, and
    // email. The bare JID is immutable, so the localpart/username/JID must
    // NOT change; only the mutable profile fields are refreshed.
    let mut claims = claims();
    claims.preferred_username = Some("rawkode".to_string());
    claims.name = Some("Rawkode".to_string());
    claims.email = Some("rawkode@waddle.social".to_string());
    claims.raw_claims = json!({
        "sub": "sub-1234",
        "preferred_username": "rawkode",
    });

    let linked = service
        .resolve_or_create_user(&provider, &claims, "waddle.test")
        .await
        .expect("resolve identity");
    assert_eq!(linked.user.jid, "example.person@waddle.test");
    assert_eq!(linked.user.username, "example.person");
    assert_eq!(linked.user.xmpp_localpart, "example.person");

    let conn = db.guard().await.expect("db guard");
    let mut rows = conn
        .query(
            "SELECT username, xmpp_localpart, display_name, primary_email FROM users WHERE jid = ?",
            crate::db_params!["example.person@waddle.test"],
        )
        .await
        .expect("query user");
    let row = rows.next().await.expect("read row").expect("row exists");
    let username: String = row.get(0).expect("username");
    let xmpp_localpart: String = row.get(1).expect("xmpp_localpart");
    let display_name: Option<String> = row.get(2).expect("display_name");
    let primary_email: Option<String> = row.get(3).expect("primary_email");
    assert_eq!(username, "example.person");
    assert_eq!(xmpp_localpart, "example.person");
    assert_eq!(display_name.as_deref(), Some("Rawkode"));
    assert_eq!(primary_email.as_deref(), Some("rawkode@waddle.social"));

    let mut rows = conn
        .query(
            "SELECT email, raw_claims_json FROM auth_identities WHERE issuer = ? AND subject = ?",
            crate::db_params!["https://issuer.example", "sub-1234"],
        )
        .await
        .expect("query identity");
    let row = rows.next().await.expect("read row").expect("row exists");
    let identity_email: Option<String> = row.get(0).expect("identity email");
    let raw_claims_json: String = row.get(1).expect("raw claims");
    assert_eq!(identity_email.as_deref(), Some("rawkode@waddle.social"));
    assert!(raw_claims_json.contains("\"preferred_username\":\"rawkode\""));
}

#[tokio::test]
async fn existing_identity_keeps_username_even_when_claim_conflicts() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let service = IdentityService::new(actor);
    let provider = provider_with_username_claim(Some("preferred_username"));

    {
        let conn = db.guard().await.expect("db guard");
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "rawkode@waddle.test",
                "rawkode",
                "rawkode",
                now.as_str(),
                now.as_str()
            ],
        )
        .await
        .expect("insert conflicting user");

        conn.execute(
            r#"
            INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "example.person@waddle.test",
                "example.person",
                "example.person",
                now.as_str(),
                now.as_str()
            ],
        )
        .await
        .expect("insert identity user");

        conn.execute(
            r#"
            INSERT INTO auth_identities (
                id, user_jid, provider_id, issuer, subject, raw_claims_json, created_at, last_login_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "identity-1",
                "example.person@waddle.test",
                "provider",
                "https://issuer.example",
                "sub-1234",
                "{}",
                now.as_str(),
                now.as_str()
            ],
        )
        .await
        .expect("insert identity");
    }

    // Even though the IDP now reports `rawkode` (which collides with another
    // user), the existing identity's immutable JID/username are preserved —
    // there is no username reallocation on login anymore.
    let mut claims = claims();
    claims.preferred_username = Some("rawkode".to_string());
    claims.raw_claims = json!({
        "sub": "sub-1234",
        "preferred_username": "rawkode",
    });

    let linked = service
        .resolve_or_create_user(&provider, &claims, "waddle.test")
        .await
        .expect("resolve identity");
    assert_eq!(linked.user.jid, "example.person@waddle.test");
    assert_eq!(linked.user.username, "example.person");
    assert_eq!(linked.user.xmpp_localpart, "example.person");
}
