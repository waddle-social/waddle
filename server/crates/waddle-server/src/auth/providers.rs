use crate::auth::AuthError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Supported provider protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthProviderKind {
    Oidc,
    OAuth2,
}

/// OAuth2 token endpoint authentication method for a provider client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthProviderTokenEndpointAuthMethod {
    /// RFC 6749 client credentials in request body.
    #[default]
    ClientSecretPost,
    /// Public client using PKCE without client authentication.
    #[serde(rename = "none")]
    NoAuthentication,
}

/// Static auth provider configuration loaded from environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProviderConfig {
    pub id: String,
    pub display_name: String,
    pub kind: AuthProviderKind,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod,
    pub scopes: Vec<String>,

    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub authorization_endpoint: Option<String>,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub jwks_uri: Option<String>,

    #[serde(default = "default_subject_claim")]
    pub subject_claim: String,
    #[serde(default)]
    pub username_claim: Option<String>,
    #[serde(default)]
    pub email_claim: Option<String>,
}

fn default_subject_claim() -> String {
    "sub".to_string()
}

impl AuthProviderConfig {
    pub fn validate(&self) -> Result<(), AuthError> {
        if self.id.trim().is_empty() {
            return Err(AuthError::InvalidRequest(
                "provider id cannot be empty".to_string(),
            ));
        }
        if self.display_name.trim().is_empty() {
            return Err(AuthError::InvalidRequest(format!(
                "provider '{}' display_name cannot be empty",
                self.id
            )));
        }
        if self.client_id.trim().is_empty() {
            return Err(AuthError::InvalidRequest(format!(
                "provider '{}' client_id cannot be empty",
                self.id
            )));
        }
        if self.token_endpoint_auth_method.requires_client_secret()
            && self.client_secret.trim().is_empty()
        {
            return Err(AuthError::InvalidRequest(format!(
                "provider '{}' client_secret cannot be empty for token_endpoint_auth_method=client_secret_post",
                self.id
            )));
        }
        if self.scopes.is_empty() {
            return Err(AuthError::InvalidRequest(format!(
                "provider '{}' scopes cannot be empty",
                self.id
            )));
        }
        if self.subject_claim.trim().is_empty() {
            return Err(AuthError::InvalidRequest(format!(
                "provider '{}' subject_claim cannot be empty",
                self.id
            )));
        }

        match self.kind {
            AuthProviderKind::Oidc => {
                if self.issuer.as_deref().unwrap_or_default().trim().is_empty() {
                    return Err(AuthError::InvalidRequest(format!(
                        "provider '{}' (oidc) requires issuer",
                        self.id
                    )));
                }
                if !self.scopes.iter().any(|s| s == "openid") {
                    return Err(AuthError::InvalidRequest(format!(
                        "provider '{}' (oidc) scopes must include 'openid'",
                        self.id
                    )));
                }
            }
            AuthProviderKind::OAuth2 => {
                if self
                    .authorization_endpoint
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    return Err(AuthError::InvalidRequest(format!(
                        "provider '{}' (oauth2) requires authorization_endpoint",
                        self.id
                    )));
                }
                if self
                    .token_endpoint
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    return Err(AuthError::InvalidRequest(format!(
                        "provider '{}' (oauth2) requires token_endpoint",
                        self.id
                    )));
                }
                if self
                    .userinfo_endpoint
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    return Err(AuthError::InvalidRequest(format!(
                        "provider '{}' (oauth2) requires userinfo_endpoint",
                        self.id
                    )));
                }
            }
        }

        Ok(())
    }

    pub fn scopes_string(&self) -> String {
        if self.scopes.is_empty() {
            "openid profile email".to_string()
        } else {
            self.scopes.join(" ")
        }
    }

    pub fn includes_client_secret_in_token_request(&self) -> bool {
        self.token_endpoint_auth_method.requires_client_secret()
    }
}

impl AuthProviderTokenEndpointAuthMethod {
    pub fn requires_client_secret(self) -> bool {
        matches!(self, AuthProviderTokenEndpointAuthMethod::ClientSecretPost)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicProvider {
    pub id: String,
    pub display_name: String,
    pub kind: AuthProviderKind,
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: HashMap<String, AuthProviderConfig>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<AuthProviderConfig>) -> Result<Self, AuthError> {
        let mut seen = HashSet::new();
        let mut map = HashMap::new();

        for provider in providers {
            provider.validate()?;
            if !seen.insert(provider.id.clone()) {
                return Err(AuthError::InvalidRequest(format!(
                    "duplicate provider id: {}",
                    provider.id
                )));
            }
            map.insert(provider.id.clone(), provider);
        }

        Ok(Self { providers: map })
    }

    pub fn get(&self, id: &str) -> Option<&AuthProviderConfig> {
        self.providers.get(id)
    }

    pub fn list(&self) -> Vec<PublicProvider> {
        let mut providers: Vec<_> = self
            .providers
            .values()
            .map(|p| PublicProvider {
                id: p.id.clone(),
                display_name: p.display_name.clone(),
                kind: p.kind,
            })
            .collect();
        providers.sort_by(|a, b| a.id.cmp(&b.id));
        providers
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oidc_provider() -> AuthProviderConfig {
        AuthProviderConfig {
            id: "google".to_string(),
            display_name: "Google".to_string(),
            kind: AuthProviderKind::Oidc,
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ],
            issuer: Some("https://accounts.google.com".to_string()),
            authorization_endpoint: None,
            token_endpoint: None,
            userinfo_endpoint: None,
            jwks_uri: None,
            subject_claim: "sub".to_string(),
            username_claim: Some("preferred_username".to_string()),
            email_claim: Some("email".to_string()),
        }
    }

    fn oauth2_provider() -> AuthProviderConfig {
        AuthProviderConfig {
            id: "github".to_string(),
            display_name: "GitHub".to_string(),
            kind: AuthProviderKind::OAuth2,
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
            scopes: vec!["read:user".to_string(), "user:email".to_string()],
            issuer: None,
            authorization_endpoint: Some("https://github.com/login/oauth/authorize".to_string()),
            token_endpoint: Some("https://github.com/login/oauth/access_token".to_string()),
            userinfo_endpoint: Some("https://api.github.com/user".to_string()),
            jwks_uri: None,
            subject_claim: "id".to_string(),
            username_claim: Some("login".to_string()),
            email_claim: Some("email".to_string()),
        }
    }

    #[test]
    fn oidc_requires_openid_scope() {
        let mut provider = oidc_provider();
        provider.scopes = vec!["profile".to_string(), "email".to_string()];
        let err = provider.validate().unwrap_err().to_string();
        assert!(err.contains("scopes must include 'openid'"));
    }

    #[test]
    fn oauth2_requires_endpoints() {
        let mut provider = oauth2_provider();
        provider.userinfo_endpoint = None;
        let err = provider.validate().unwrap_err().to_string();
        assert!(err.contains("requires userinfo_endpoint"));
    }

    #[test]
    fn provider_scopes_must_not_be_empty() {
        let mut provider = oidc_provider();
        provider.scopes.clear();
        let err = provider.validate().unwrap_err().to_string();
        assert!(err.contains("scopes cannot be empty"));
    }

    #[test]
    fn client_secret_post_requires_client_secret() {
        let mut provider = oidc_provider();
        provider.client_secret = "".to_string();
        provider.token_endpoint_auth_method = AuthProviderTokenEndpointAuthMethod::ClientSecretPost;
        let err = provider.validate().unwrap_err().to_string();
        assert!(err.contains("client_secret cannot be empty"));
    }

    #[test]
    fn none_auth_allows_empty_client_secret() {
        let mut provider = oidc_provider();
        provider.client_secret = "".to_string();
        provider.token_endpoint_auth_method = AuthProviderTokenEndpointAuthMethod::NoAuthentication;
        provider.validate().expect("public client should be valid");
    }

    #[test]
    fn registry_rejects_duplicate_provider_ids() {
        let providers = vec![oidc_provider(), oidc_provider()];
        let err = ProviderRegistry::new(providers).unwrap_err().to_string();
        assert!(err.contains("duplicate provider id"));
    }

    #[test]
    fn registry_accepts_valid_mixed_providers() {
        let providers = vec![oidc_provider(), oauth2_provider()];
        let registry = ProviderRegistry::new(providers).expect("registry should build");
        assert!(!registry.is_empty());
        assert_eq!(registry.list().len(), 2);
        assert!(registry.get("google").is_some());
        assert!(registry.get("github").is_some());
    }

    #[test]
    fn deserialize_public_client_without_client_secret() {
        let raw = r#"{
            "id":"rawkode",
            "display_name":"rawkode.academy",
            "kind":"oidc",
            "client_id":"public-client",
            "token_endpoint_auth_method":"none",
            "scopes":["openid","profile","email"],
            "issuer":"https://id.rawkode.academy/auth",
            "subject_claim":"sub"
        }"#;

        let provider: AuthProviderConfig =
            serde_json::from_str(raw).expect("provider should deserialize");
        provider.validate().expect("provider should validate");
        assert_eq!(
            provider.token_endpoint_auth_method,
            AuthProviderTokenEndpointAuthMethod::NoAuthentication
        );
        assert!(provider.client_secret.is_empty());
    }
}
