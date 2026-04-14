use crate::auth::{AuthError, AuthProviderConfig, IdentityClaims};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

pub async fn exchange_code(
    client: &Client,
    provider: &AuthProviderConfig,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthTokenResponse, AuthError> {
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("client_id", provider.client_id.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];
    if provider.includes_client_secret_in_token_request() {
        params.push(("client_secret", provider.client_secret.as_str()));
    }

    let res = client
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| AuthError::TokenExchangeFailed(e.to_string()))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::TokenExchangeFailed(format!(
            "token endpoint {}: {}",
            status, body
        )));
    }

    let token = res
        .json::<OAuthTokenResponse>()
        .await
        .map_err(|e| AuthError::TokenExchangeFailed(format!("Invalid token response: {}", e)))?;

    Ok(token)
}

pub async fn fetch_userinfo(
    client: &Client,
    endpoint: &str,
    access_token: &str,
) -> Result<Value, AuthError> {
    let res = client
        .get(endpoint)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| AuthError::UserInfoFailed(e.to_string()))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::UserInfoFailed(format!(
            "userinfo endpoint {}: {}",
            status, body
        )));
    }

    res.json::<Value>()
        .await
        .map_err(|e| AuthError::UserInfoFailed(format!("Invalid userinfo response: {}", e)))
}

fn value_string(value: Option<&Value>) -> Option<String> {
    value.and_then(|v| {
        if let Some(s) = v.as_str() {
            Some(s.to_string())
        } else if v.is_number() || v.is_boolean() {
            Some(v.to_string())
        } else {
            None
        }
    })
}

fn value_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(|v| {
        if let Some(b) = v.as_bool() {
            Some(b)
        } else if let Some(s) = v.as_str() {
            match s.to_lowercase().as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            }
        } else {
            None
        }
    })
}

pub fn claims_from_userinfo(
    provider: &AuthProviderConfig,
    issuer: Option<String>,
    userinfo: Value,
) -> Result<IdentityClaims, AuthError> {
    let subject = value_string(userinfo.get(&provider.subject_claim)).ok_or_else(|| {
        AuthError::InvalidRequest(format!(
            "userinfo missing subject claim '{}'",
            provider.subject_claim
        ))
    })?;

    let preferred_username = provider
        .username_claim
        .as_deref()
        .and_then(|k| value_string(userinfo.get(k)))
        .or_else(|| value_string(userinfo.get("preferred_username")))
        .or_else(|| value_string(userinfo.get("login")));

    let email = provider
        .email_claim
        .as_deref()
        .and_then(|k| value_string(userinfo.get(k)))
        .or_else(|| value_string(userinfo.get("email")));

    Ok(IdentityClaims {
        subject,
        issuer,
        preferred_username,
        name: value_string(userinfo.get("name")),
        email,
        email_verified: value_bool(userinfo.get("email_verified")),
        avatar_url: value_string(userinfo.get("picture"))
            .or_else(|| value_string(userinfo.get("avatar_url"))),
        raw_claims: userinfo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthProviderKind, AuthProviderTokenEndpointAuthMethod};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn oidc_provider_for(auth_method: AuthProviderTokenEndpointAuthMethod) -> AuthProviderConfig {
        AuthProviderConfig {
            id: "rawkode".to_string(),
            display_name: "rawkode.academy".to_string(),
            kind: AuthProviderKind::Oidc,
            client_id: "public-client".to_string(),
            client_secret: "super-secret".to_string(),
            token_endpoint_auth_method: auth_method,
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ],
            issuer: Some("https://id.rawkode.academy/auth".to_string()),
            authorization_endpoint: None,
            token_endpoint: Some("https://id.rawkode.academy/auth/token".to_string()),
            userinfo_endpoint: Some("https://id.rawkode.academy/auth/userinfo".to_string()),
            jwks_uri: None,
            subject_claim: "sub".to_string(),
            username_claim: Some("preferred_username".to_string()),
            email_claim: Some("email".to_string()),
        }
    }

    #[tokio::test]
    async fn exchange_code_includes_client_secret_for_client_secret_post() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-token",
                "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let provider = oidc_provider_for(AuthProviderTokenEndpointAuthMethod::ClientSecretPost);
        let token_endpoint = format!("{}/token", mock_server.uri());
        let _ = exchange_code(
            &Client::new(),
            &provider,
            &token_endpoint,
            "auth-code",
            "https://app.example/callback",
            "pkce-verifier",
        )
        .await
        .expect("token exchange should succeed");

        let requests = mock_server
            .received_requests()
            .await
            .expect("received requests should be available");
        let body =
            String::from_utf8(requests[0].body.clone()).expect("request body should be utf8");
        assert!(body.contains("client_secret=super-secret"));
    }

    #[tokio::test]
    async fn exchange_code_omits_client_secret_for_public_client() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-token",
                "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let provider = oidc_provider_for(AuthProviderTokenEndpointAuthMethod::NoAuthentication);
        let token_endpoint = format!("{}/token", mock_server.uri());
        let _ = exchange_code(
            &Client::new(),
            &provider,
            &token_endpoint,
            "auth-code",
            "https://app.example/callback",
            "pkce-verifier",
        )
        .await
        .expect("token exchange should succeed");

        let requests = mock_server
            .received_requests()
            .await
            .expect("received requests should be available");
        let body =
            String::from_utf8(requests[0].body.clone()).expect("request body should be utf8");
        assert!(!body.contains("client_secret="));
    }
}
