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
    #[serde(default)]
    pub client_secret: String,
    pub token_endpoint_auth_method: String,
}

pub enum UserinfoFetchOutcome {
    NotRequested,
    Succeeded,
    Failed(AuthError),
}

pub struct OidcClaimsOutcome {
    pub claims: IdentityClaims,
    pub userinfo: UserinfoFetchOutcome,
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
        .map_err(|e| AuthError::HttpError(e.to_string()))?;

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
    require_dpop: bool,
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
        require_dpop,
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
        client_name: "waddle-server".to_string(),
        scope: provider.scopes_string(),
        metadata: serde_json::json!({
            "product": "server",
            "providerId": provider.id,
            "requireDpop": provider.require_dpop,
        }),
    };

    let res = client
        .post(registration_endpoint)
        .json(&req)
        .send()
        .await
        .map_err(|e| AuthError::HttpError(e.to_string()))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::AuthorizationFailed(format!(
            "dynamic registration {}: {}",
            status, body
        )));
    }

    let mut payload = res.json::<DynamicClientRegistration>().await.map_err(|e| {
        AuthError::AuthorizationFailed(format!("Invalid registration payload: {}", e))
    })?;

    if payload.client_id.trim().is_empty() {
        return Err(AuthError::AuthorizationFailed(
            "dynamic registration returned empty client_id".to_string(),
        ));
    }
    let token_endpoint_auth_method = payload.token_endpoint_auth_method.trim().to_string();
    if token_endpoint_auth_method.is_empty() {
        return Err(AuthError::AuthorizationFailed(
            "dynamic registration returned empty token_endpoint_auth_method".to_string(),
        ));
    }
    if token_endpoint_auth_method != "none" && payload.client_secret.trim().is_empty() {
        return Err(AuthError::AuthorizationFailed(
            "dynamic registration returned empty client_secret".to_string(),
        ));
    }
    payload.token_endpoint_auth_method = token_endpoint_auth_method;

    Ok(payload)
}

async fn fetch_jwks(client: &Client, jwks_uri: &str) -> Result<JwkSet, AuthError> {
    let res = client
        .get(jwks_uri)
        .send()
        .await
        .map_err(|e| AuthError::HttpError(e.to_string()))?;

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

pub(crate) fn avatar_url_from_claims(claims: &Value) -> Option<String> {
    value_string(claims.get("picture"))
        .or_else(|| value_string(claims.get("avatar_url")))
        .or_else(|| value_string(claims.get("profile")))
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
) -> Result<OidcClaimsOutcome, AuthError> {
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
    let userinfo = if let Some(userinfo_endpoint) = provider
        .userinfo_endpoint
        .as_deref()
        .or(discovery.userinfo_endpoint.as_deref())
    {
        match fetch_userinfo(client, userinfo_endpoint, &token.access_token).await {
            Ok(userinfo) => {
                if let Some(obj) = merged.as_object_mut() {
                    if let Some(userinfo_obj) = userinfo.as_object() {
                        for (k, v) in userinfo_obj {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                }
                UserinfoFetchOutcome::Succeeded
            }
            Err(error) => UserinfoFetchOutcome::Failed(error),
        }
    } else {
        UserinfoFetchOutcome::NotRequested
    };

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

    Ok(OidcClaimsOutcome {
        claims: IdentityClaims {
            subject,
            issuer: Some(discovery.issuer.clone()),
            preferred_username,
            name: value_string(merged.get("name")),
            email,
            email_verified: value_bool(merged.get("email_verified")),
            avatar_url: avatar_url_from_claims(&merged),
            raw_claims: merged,
        },
        userinfo,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthProviderKind, AuthProviderTokenEndpointAuthMethod};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn dynamic_oidc_provider(
        token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod,
    ) -> AuthProviderConfig {
        AuthProviderConfig {
            id: "colony".to_string(),
            display_name: "Colony".to_string(),
            kind: AuthProviderKind::Oidc,
            dynamic_client_registration: true,
            client_id: "".to_string(),
            client_secret: "".to_string(),
            token_endpoint_auth_method,
            require_dpop: true,
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ],
            issuer: Some("https://colony.waddle.social".to_string()),
            authorization_endpoint: None,
            token_endpoint: None,
            userinfo_endpoint: None,
            jwks_uri: None,
            subject_claim: "sub".to_string(),
            username_claim: Some("preferred_username".to_string()),
            email_claim: Some("email".to_string()),
        }
    }

    fn discovery(registration_endpoint: String) -> OidcDiscovery {
        OidcDiscovery {
            issuer: "https://colony.waddle.social".to_string(),
            authorization_endpoint: "https://colony.waddle.social/api/auth/oauth2/authorize"
                .to_string(),
            token_endpoint: "https://colony.waddle.social/api/auth/oauth2/token".to_string(),
            userinfo_endpoint: Some(
                "https://colony.waddle.social/api/auth/oauth2/userinfo".to_string(),
            ),
            jwks_uri: "https://colony.waddle.social/api/auth/jwks".to_string(),
            registration_endpoint: Some(registration_endpoint),
        }
    }

    #[tokio::test]
    async fn dynamic_registration_accepts_public_client_without_secret() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "client_id": "registered-public-client",
                "token_endpoint_auth_method": "none"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let registration = register_dynamic_client(
            &Client::new(),
            &dynamic_oidc_provider(AuthProviderTokenEndpointAuthMethod::NoAuthentication),
            &discovery(format!("{}/register", mock_server.uri())),
            "https://waddle.social/api/auth/callback",
        )
        .await
        .expect("public DCR response should be accepted");

        assert_eq!(registration.client_id, "registered-public-client");
        assert_eq!(registration.token_endpoint_auth_method, "none");
        assert!(registration.client_secret.is_empty());
    }

    #[tokio::test]
    async fn dynamic_registration_requires_secret_for_confidential_client() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "client_id": "registered-confidential-client",
                "token_endpoint_auth_method": "client_secret_post"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let err = register_dynamic_client(
            &Client::new(),
            &dynamic_oidc_provider(AuthProviderTokenEndpointAuthMethod::ClientSecretPost),
            &discovery(format!("{}/register", mock_server.uri())),
            "https://waddle.social/api/auth/callback",
        )
        .await
        .expect_err("confidential DCR response without secret should fail")
        .to_string();

        assert!(err.contains("dynamic registration returned empty client_secret"));
    }

    #[test]
    fn avatar_url_prefers_picture_claim() {
        let claims = json!({
            "picture": "https://cdn.example.com/picture.png",
            "profile": "https://cdn.example.com/profile.png"
        });

        assert_eq!(
            avatar_url_from_claims(&claims).as_deref(),
            Some("https://cdn.example.com/picture.png")
        );
    }

    #[test]
    fn avatar_url_falls_back_to_profile_claim() {
        let claims = json!({
            "profile": "https://cdn.example.com/profile.png"
        });

        assert_eq!(
            avatar_url_from_claims(&claims).as_deref(),
            Some("https://cdn.example.com/profile.png")
        );
    }
}
