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
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", provider.client_id.as_str()),
        ("client_secret", provider.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];

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
