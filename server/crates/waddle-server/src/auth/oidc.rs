use crate::auth::oauth2::{
    claims_from_userinfo, exchange_code, fetch_userinfo, OAuthTokenResponse,
};
use crate::auth::{AuthError, AuthProviderConfig, IdentityClaims};
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: Option<String>,
    pub jwks_uri: String,
    pub registration_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicClientRegistration {
    pub client_id: String,
    pub client_secret: String,
    pub token_endpoint_auth_method: String,
}

#[derive(Debug, Serialize)]
struct DynamicRegistrationRequest {
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    token_endpoint_auth_method: String,
    client_name: String,
    scope: String,
    metadata: Value,
}

pub async fn discover(client: &Client, issuer: &str) -> Result<OidcDiscovery, AuthError> {
    let issuer = issuer.trim_end_matches('/');
    let url = format!("{}/.well-known/openid-configuration", issuer);

    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AuthError::AuthorizationFailed(e.to_string()))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::AuthorizationFailed(format!(
            "oidc discovery {}: {}",
            status, body
        )));
    }

    res.json::<OidcDiscovery>()
        .await
        .map_err(|e| AuthError::AuthorizationFailed(format!("Invalid OIDC discovery: {}", e)))
}

pub async fn exchange_authorization_code(
    client: &Client,
    provider: &AuthProviderConfig,
    discovery: &OidcDiscovery,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    dpop_proof: Option<&str>,
) -> Result<OAuthTokenResponse, AuthError> {
    let token_endpoint = provider
        .token_endpoint
        .as_deref()
        .unwrap_or(&discovery.token_endpoint);

    exchange_code(
        client,
        provider,
        token_endpoint,
        code,
        redirect_uri,
        code_verifier,
        dpop_proof,
    )
    .await
}

pub async fn register_dynamic_client(
    client: &Client,
    provider: &AuthProviderConfig,
    discovery: &OidcDiscovery,
    redirect_uri: &str,
) -> Result<DynamicClientRegistration, AuthError> {
    let registration_endpoint = discovery.registration_endpoint.as_deref().ok_or_else(|| {
        AuthError::InvalidRequest(format!(
            "provider '{}' discovery missing registration_endpoint",
            provider.id
        ))
    })?;

    let requested_auth_method = match provider.token_endpoint_auth_method {
        crate::auth::providers::AuthProviderTokenEndpointAuthMethod::ClientSecretPost => {
            "client_secret_post"
        }
        crate::auth::providers::AuthProviderTokenEndpointAuthMethod::NoAuthentication => "none",
    };

    let req = DynamicRegistrationRequest {
        redirect_uris: vec![redirect_uri.to_string()],
        grant_types: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ],
        response_types: vec!["code".to_string()],
        token_endpoint_auth_method: requested_auth_method.to_string(),
        client_name: provider.display_name.clone(),
        scope: provider.scopes_string(),
        metadata: serde_json::json!({
            "providerId": provider.id,
            "requireDpop": provider.require_dpop,
        }),
    };

    let res = client
        .post(registration_endpoint)
        .json(&req)
        .send()
        .await
        .map_err(|e| AuthError::AuthorizationFailed(e.to_string()))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::AuthorizationFailed(format!(
            "dynamic registration {}: {}",
            status, body
        )));
    }

    let payload = res.json::<DynamicClientRegistration>().await.map_err(|e| {
        AuthError::AuthorizationFailed(format!("Invalid registration payload: {}", e))
    })?;

    if payload.client_id.trim().is_empty() {
        return Err(AuthError::AuthorizationFailed(
            "dynamic registration returned empty client_id".to_string(),
        ));
    }
    if payload.client_secret.trim().is_empty() {
        return Err(AuthError::AuthorizationFailed(
            "dynamic registration returned empty client_secret".to_string(),
        ));
    }
    if payload.token_endpoint_auth_method.trim().is_empty() {
        return Err(AuthError::AuthorizationFailed(
            "dynamic registration returned empty token_endpoint_auth_method".to_string(),
        ));
    }

    Ok(payload)
}

async fn fetch_jwks(client: &Client, jwks_uri: &str) -> Result<JwkSet, AuthError> {
    let res = client
        .get(jwks_uri)
        .send()
        .await
        .map_err(|e| AuthError::JwtError(e.to_string()))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::JwtError(format!(
            "jwks endpoint {}: {}",
            status, body
        )));
    }

    res.json::<JwkSet>()
        .await
        .map_err(|e| AuthError::JwtError(format!("Invalid JWKS payload: {}", e)))
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

fn select_jwk<'a>(jwks: &'a JwkSet, kid: Option<&str>) -> Option<&'a Jwk> {
    if let Some(kid) = kid {
        if let Some(found) = jwks
            .keys
            .iter()
            .find(|jwk| jwk.common.key_id.as_deref() == Some(kid))
        {
            return Some(found);
        }
    }
    jwks.keys.first()
}

pub async fn validate_id_token(
    client: &Client,
    provider: &AuthProviderConfig,
    discovery: &OidcDiscovery,
    id_token: &str,
) -> Result<Value, AuthError> {
    let header = decode_header(id_token)?;
    let jwks_uri = provider.jwks_uri.as_deref().unwrap_or(&discovery.jwks_uri);
    let jwks = fetch_jwks(client, jwks_uri).await?;

    let jwk = select_jwk(&jwks, header.kid.as_deref())
        .ok_or_else(|| AuthError::JwtError("no jwk available for token".to_string()))?;

    let key = DecodingKey::from_jwk(jwk)?;

    let mut validation = Validation::new(header.alg);
    validation.set_audience(&[provider.client_id.as_str()]);
    validation.set_issuer(&[discovery.issuer.as_str()]);
    validation.validate_exp = true;
    validation.validate_nbf = true;

    let decoded = decode::<Value>(id_token, &key, &validation)?;
    Ok(decoded.claims)
}

pub async fn claims_from_token_response(
    client: &Client,
    provider: &AuthProviderConfig,
    discovery: &OidcDiscovery,
    token: &OAuthTokenResponse,
    expected_nonce: Option<&str>,
) -> Result<IdentityClaims, AuthError> {
    let id_token = token.id_token.as_deref().ok_or_else(|| {
        AuthError::InvalidRequest("OIDC provider did not return id_token".to_string())
    })?;

    let id_claims = validate_id_token(client, provider, discovery, id_token).await?;
    if let Some(expected_nonce) = expected_nonce {
        let Some(token_nonce) = value_string(id_claims.get("nonce")) else {
            return Err(AuthError::InvalidNonce);
        };
        if token_nonce != expected_nonce {
            return Err(AuthError::InvalidNonce);
        }
    }

    let subject = value_string(id_claims.get(&provider.subject_claim)).ok_or_else(|| {
        AuthError::InvalidRequest(format!(
            "id_token missing subject claim '{}'",
            provider.subject_claim
        ))
    })?;

    let mut merged = id_claims.clone();

    // If userinfo endpoint exists, merge extra profile claims on top.
    if let Some(userinfo_endpoint) = provider
        .userinfo_endpoint
        .as_deref()
        .or(discovery.userinfo_endpoint.as_deref())
    {
        if let Ok(userinfo) = fetch_userinfo(client, userinfo_endpoint, &token.access_token).await {
            if let Some(obj) = merged.as_object_mut() {
                if let Some(userinfo_obj) = userinfo.as_object() {
                    for (k, v) in userinfo_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }

    let preferred_username = provider
        .username_claim
        .as_deref()
        .and_then(|k| value_string(merged.get(k)))
        .or_else(|| value_string(merged.get("preferred_username")))
        .or_else(|| value_string(merged.get("login")));

    let email = provider
        .email_claim
        .as_deref()
        .and_then(|k| value_string(merged.get(k)))
        .or_else(|| value_string(merged.get("email")));

    Ok(IdentityClaims {
        subject,
        issuer: Some(discovery.issuer.clone()),
        preferred_username,
        name: value_string(merged.get("name")),
        email,
        email_verified: value_bool(merged.get("email_verified")),
        avatar_url: value_string(merged.get("picture"))
            .or_else(|| value_string(merged.get("avatar_url"))),
        raw_claims: merged,
    })
}

pub async fn claims_from_oauth2_fallback(
    client: &Client,
    provider: &AuthProviderConfig,
    issuer: Option<String>,
    access_token: &str,
    userinfo_endpoint: &str,
) -> Result<IdentityClaims, AuthError> {
    let userinfo = fetch_userinfo(client, userinfo_endpoint, access_token).await?;
    claims_from_userinfo(provider, issuer, userinfo)
}
