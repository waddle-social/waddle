use super::*;
use crate::auth::{AuthProviderKind, AuthProviderTokenEndpointAuthMethod};
use crate::db::{Database, MigrationRunner};
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
async fn existing_identity_updates_username_and_claims_on_login() {
    let db = create_test_db().await;
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let service = IdentityService::new(actor);
    let provider = provider_with_username_claim(Some("preferred_username"));

    {
        let conn = db.guard().await.expect("db guard");
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO users (id, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "user-1",
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
                id, user_id, provider_id, issuer, subject, email, email_verified,
                raw_claims_json, created_at, last_login_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "identity-1",
                "user-1",
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

    let mut claims = claims();
    claims.preferred_username = Some("rawkode".to_string());
    claims.name = Some("Rawkode".to_string());
    claims.email = Some("rawkode@waddle.social".to_string());
    claims.raw_claims = json!({
        "sub": "sub-1234",
        "preferred_username": "rawkode",
    });

    let linked = service
        .resolve_or_create_user(&provider, &claims)
        .await
        .expect("resolve identity");
    assert_eq!(linked.user.username, "rawkode");
    assert_eq!(linked.user.xmpp_localpart, "rawkode");

    let conn = db.guard().await.expect("db guard");
    let mut rows = conn
        .query(
            "SELECT username, xmpp_localpart, primary_email FROM users WHERE id = ?",
            crate::db_params!["user-1"],
        )
        .await
        .expect("query user");
    let row = rows.next().await.expect("read row").expect("row exists");
    let username: String = row.get(0).expect("username");
    let xmpp_localpart: String = row.get(1).expect("xmpp_localpart");
    let primary_email: Option<String> = row.get(2).expect("primary_email");
    assert_eq!(username, "rawkode");
    assert_eq!(xmpp_localpart, "rawkode");
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
async fn existing_identity_uses_suffix_when_username_conflicts() {
    let db = create_test_db().await;
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let service = IdentityService::new(actor);
    let provider = provider_with_username_claim(Some("preferred_username"));

    {
        let conn = db.guard().await.expect("db guard");
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO users (id, username, xmpp_localpart, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "user-conflict",
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
            INSERT INTO users (id, username, xmpp_localpart, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "user-1",
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
                id, user_id, provider_id, issuer, subject, raw_claims_json, created_at, last_login_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "identity-1",
                "user-1",
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

    let mut claims = claims();
    claims.preferred_username = Some("rawkode".to_string());
    claims.raw_claims = json!({
        "sub": "sub-1234",
        "preferred_username": "rawkode",
    });

    let linked = service
        .resolve_or_create_user(&provider, &claims)
        .await
        .expect("resolve identity");
    assert_eq!(linked.user.username, "rawkode1");
    assert_eq!(linked.user.xmpp_localpart, "rawkode1");
}
