use crate::auth::{AuthError, AuthProviderConfig, IdentityClaims};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::rand_core::OsRng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

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
    require_dpop: bool,
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

    let body = encode_form(&params);
    let mut request = client
        .post(token_endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body);
    if require_dpop {
        let dpop_proof = create_dpop_proof("POST", token_endpoint)?;
        request = request.header("DPoP", dpop_proof);
    }

    let res = request
        .send()
        .await
        .map_err(|e| AuthError::HttpError(e.to_string()))?;

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

fn encode_form(params: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in params.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&urlencoding::encode(k));
        out.push('=');
        out.push_str(&urlencoding::encode(v));
    }
    out
}

fn create_dpop_proof(method: &str, htu: &str) -> Result<String, AuthError> {
    let signing_key = SigningKey::random(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let point = verifying_key.to_encoded_point(false);
    let x = point
        .x()
        .ok_or_else(|| AuthError::TokenExchangeFailed("missing EC x coordinate".to_string()))?;
    let y = point
        .y()
        .ok_or_else(|| AuthError::TokenExchangeFailed("missing EC y coordinate".to_string()))?;

    let header = serde_json::json!({
        "typ": "dpop+jwt",
        "alg": "ES256",
        "jwk": {
            "kty": "EC",
            "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(x),
            "y": URL_SAFE_NO_PAD.encode(y),
        }
    });

    let issued_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| AuthError::TokenExchangeFailed(format!("invalid system time: {}", e)))?
        .as_secs() as i64;
    let payload = serde_json::json!({
        "htm": method.to_uppercase(),
        "htu": htu,
        "iat": issued_at,
        "jti": Uuid::new_v4().to_string(),
    });

    let header_encoded = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&header)
            .map_err(|e| AuthError::TokenExchangeFailed(format!("invalid DPoP header: {}", e)))?,
    );
    let payload_encoded = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&payload)
            .map_err(|e| AuthError::TokenExchangeFailed(format!("invalid DPoP payload: {}", e)))?,
    );
    let signing_input = format!("{}.{}", header_encoded, payload_encoded);

    let signature: Signature = signing_key.sign(signing_input.as_bytes());
    let signature_encoded = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    Ok(format!(
        "{}.{}.{}",
        header_encoded, payload_encoded, signature_encoded
    ))
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
        .map_err(|e| AuthError::HttpError(e.to_string()))?;

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
            dynamic_client_registration: false,
            client_id: "public-client".to_string(),
            client_secret: "super-secret".to_string(),
            token_endpoint_auth_method: auth_method,
            require_dpop: false,
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
            false,
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
            false,
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

    #[tokio::test]
    async fn exchange_code_sends_valid_dpop_header_when_required() {
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
            true,
        )
        .await
        .expect("token exchange should succeed");

        let requests = mock_server
            .received_requests()
            .await
            .expect("received requests should be available");
        let dpop = requests[0]
            .headers
            .get("dpop")
            .and_then(|value| value.to_str().ok())
            .expect("DPoP header should be present");
        let mut parts = dpop.split('.');
        let header = parts.next().expect("header part exists");
        let payload = parts.next().expect("payload part exists");
        let signature = parts.next().expect("signature part exists");
        assert!(parts.next().is_none(), "DPoP must have 3 JWT parts");
        assert!(!signature.is_empty(), "signature must be present");

        let header_bytes = URL_SAFE_NO_PAD
            .decode(header)
            .expect("header should be base64url");
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .expect("payload should be base64url");
        let header_json: serde_json::Value =
            serde_json::from_slice(&header_bytes).expect("header should be JSON");
        let payload_json: serde_json::Value =
            serde_json::from_slice(&payload_bytes).expect("payload should be JSON");
        assert_eq!(header_json["typ"], "dpop+jwt");
        assert_eq!(header_json["alg"], "ES256");
        assert_eq!(payload_json["htm"], "POST");
        assert_eq!(payload_json["htu"], token_endpoint);
        assert!(payload_json["jti"].as_str().is_some_and(|v| !v.is_empty()));
    }
}
