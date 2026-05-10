use std::sync::OnceLock;

use super::*;

/// Typed callback the OIDC callback handler fires after a successful
/// login to materialize a conformant PEP avatar + vCard set. The hook
/// implementation registers the publish chain with the
/// `profile_publish_tracker` (`tokio_util::task::TaskTracker`)
/// internally so login latency is unaffected AND the
/// graceful-shutdown drain can `wait()` on in-flight publishes
/// before tearing down the runtime.
pub type ProfilePublishHook =
    Arc<dyn Fn(jid::BareJid, crate::profile::ProfileSource) + Send + Sync + 'static>;

/// Shared auth state.
pub struct AuthState {
    pub session_manager: SessionManager,
    pub identity_service: IdentityService,
    pub providers: ProviderRegistry,
    pub base_url: String,
    pub xmpp_domain: String,
    /// Pre-parsed XMPP domain JID for server-issued IQ stanzas
    /// (e.g. XEP-0115 caps disco#info queries). Validated at startup
    /// so downstream construction sites cannot panic on a malformed
    /// `WADDLE_XMPP_DOMAIN` value at runtime.
    pub caps_server_domain: crate::server::caps_resolution::ServerDomainJid,
    pub http_client: reqwest::Client,
    pub pending_auth: Arc<DashMap<String, PendingAuthorization>>,
    pub device_auth: Arc<DashMap<String, DeviceAuthorization>>,
    pub xmpp_auth_codes: Arc<DashMap<String, XmppAuthCode>>,
    pub dynamic_oidc_clients: Arc<DashMap<String, oidc::DynamicClientRegistration>>,
    pub dynamic_oidc_client_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    pub permission_actor: ActorRef<PermissionActor>,
    pub bootstrap_membership: BootstrapMembershipConfig,
    /// Set once at startup after `WebSocketState` exists. The callback
    /// handler reads it via `profile_publish_hook()` and spawns it on
    /// every successful OIDC login.
    profile_publish_hook: OnceLock<ProfilePublishHook>,
}

impl AuthState {
    pub fn new(
        app_state: Arc<AppState>,
        server_config: &ServerConfig,
        encryption_key: Option<&[u8]>,
    ) -> Self {
        let session_manager =
            SessionManager::new(app_state.db_pool.global_actor().clone(), encryption_key);
        let identity_service = IdentityService::new(app_state.db_pool.global_actor().clone());
        let providers = ProviderRegistry::new(server_config.auth.providers.clone())
            .unwrap_or_else(|e| panic!("invalid provider config at startup: {}", e));

        let xmpp_domain =
            std::env::var("WADDLE_XMPP_DOMAIN").unwrap_or_else(|_| "localhost".to_string());
        let caps_server_domain = crate::server::caps_resolution::ServerDomainJid::parse(
            &xmpp_domain,
        )
        .unwrap_or_else(|error| {
            panic!("WADDLE_XMPP_DOMAIN={xmpp_domain:?} is not a valid JID at startup: {error}")
        });
        Self {
            session_manager,
            identity_service,
            providers,
            base_url: server_config.base_url.trim_end_matches('/').to_string(),
            xmpp_domain,
            caps_server_domain,
            http_client: reqwest::Client::new(),
            pending_auth: Arc::new(DashMap::new()),
            device_auth: Arc::new(DashMap::new()),
            xmpp_auth_codes: Arc::new(DashMap::new()),
            dynamic_oidc_clients: Arc::new(DashMap::new()),
            dynamic_oidc_client_locks: Arc::new(DashMap::new()),
            permission_actor: app_state.permission_actor.clone(),
            bootstrap_membership: BootstrapMembershipConfig::from_env(),
            profile_publish_hook: OnceLock::new(),
        }
    }

    /// Install the profile-publish hook. Idempotent (subsequent calls
    /// are no-ops). Called at HTTP bootstrap once `WebSocketState` and
    /// its `pubsub_storage`/`vcard_store` deps exist.
    pub fn install_profile_publish_hook(&self, hook: ProfilePublishHook) {
        let _ = self.profile_publish_hook.set(hook);
    }

    pub fn profile_publish_hook(&self) -> Option<&ProfilePublishHook> {
        self.profile_publish_hook.get()
    }

    async fn resolve_dynamic_oidc_registration(
        &self,
        provider: &AuthProviderConfig,
        discovery: &oidc::OidcDiscovery,
        redirect_uri: &str,
    ) -> Result<oidc::DynamicClientRegistration, AuthError> {
        if let Some(cached) = self.dynamic_oidc_clients.get(&provider.id) {
            return Ok(cached.clone());
        }

        let lock = self
            .dynamic_oidc_client_locks
            .entry(provider.id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        if let Some(cached) = self.dynamic_oidc_clients.get(&provider.id) {
            return Ok(cached.clone());
        }

        let registered =
            oidc::register_dynamic_client(&self.http_client, provider, discovery, redirect_uri)
                .await?;
        self.dynamic_oidc_clients
            .insert(provider.id.clone(), registered.clone());
        Ok(registered)
    }

    fn callback_url(&self) -> String {
        format!("{}/api/auth/callback", self.base_url)
    }

    pub(crate) fn websocket_url(&self) -> String {
        let parsed = match url::Url::parse(&self.base_url) {
            Ok(parsed) => parsed,
            Err(_) => return "ws://localhost/ws".to_string(),
        };

        let scheme = match parsed.scheme() {
            "https" => "wss",
            _ => "ws",
        };

        let Some(host) = parsed.host_str() else {
            return "ws://localhost/ws".to_string();
        };

        let authority = match parsed.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };

        format!("{scheme}://{authority}/ws")
    }

    pub(super) fn session_cookie_header(&self, session_id: Option<&str>, max_age: i64) -> String {
        let mut parts = vec![
            format!("waddle_session={}", session_id.unwrap_or_default()),
            "Path=/".to_string(),
            "HttpOnly".to_string(),
            "SameSite=Lax".to_string(),
            format!("Max-Age={max_age}"),
        ];

        if self.base_url.starts_with("https://") {
            parts.push("Secure".to_string());
        }

        parts.join("; ")
    }

    fn create_pkce_verifier() -> String {
        let bytes: [u8; 32] = rand::rng().random();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn pkce_challenge(verifier: &str) -> String {
        let digest = Sha256::digest(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }

    fn random_state() -> String {
        let bytes: [u8; 24] = rand::rng().random();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub async fn start_authorization(
        &self,
        provider: &AuthProviderConfig,
        flow: PendingFlow,
    ) -> Result<String, AuthError> {
        let state = Self::random_state();
        let nonce = Self::random_state();
        let code_verifier = Self::create_pkce_verifier();
        let code_challenge = Self::pkce_challenge(&code_verifier);
        let redirect_uri = self.callback_url();
        let mut client_id = provider.client_id.clone();
        let mut client_secret = provider.client_secret.clone();
        let mut token_endpoint_auth_method = provider.token_endpoint_auth_method;

        let oidc_discovery = match provider.kind {
            AuthProviderKind::Oidc => Some(
                oidc::discover(
                    &self.http_client,
                    provider.issuer.as_deref().ok_or_else(|| {
                        AuthError::InvalidRequest("oidc provider missing issuer".to_string())
                    })?,
                )
                .await?,
            ),
            AuthProviderKind::OAuth2 => None,
        };

        let authorization_endpoint = match provider.kind {
            AuthProviderKind::Oidc => {
                provider.authorization_endpoint.clone().unwrap_or_else(|| {
                    oidc_discovery
                        .as_ref()
                        .expect("oidc discovery exists for oidc providers")
                        .authorization_endpoint
                        .clone()
                })
            }
            AuthProviderKind::OAuth2 => {
                provider.authorization_endpoint.clone().ok_or_else(|| {
                    AuthError::InvalidRequest(
                        "oauth2 provider missing authorization_endpoint".to_string(),
                    )
                })?
            }
        };

        if matches!(provider.kind, AuthProviderKind::Oidc) && provider.dynamic_client_registration {
            let discovery = oidc_discovery.as_ref().ok_or_else(|| {
                AuthError::InvalidRequest("oidc discovery unavailable for provider".to_string())
            })?;
            let registration = self
                .resolve_dynamic_oidc_registration(provider, discovery, &redirect_uri)
                .await?;

            client_id = registration.client_id;
            client_secret = registration.client_secret;
            token_endpoint_auth_method = match registration.token_endpoint_auth_method.as_str() {
                "client_secret_post" => AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
                "none" => AuthProviderTokenEndpointAuthMethod::NoAuthentication,
                other => {
                    return Err(AuthError::InvalidRequest(format!(
                        "unsupported dynamic token_endpoint_auth_method '{}'",
                        other
                    )));
                }
            };
        }

        let mut url = url::Url::parse(&authorization_endpoint).map_err(|e| {
            AuthError::InvalidRequest(format!("invalid authorization endpoint: {}", e))
        })?;

        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("response_type", "code");
            qp.append_pair("client_id", &client_id);
            qp.append_pair("redirect_uri", &redirect_uri);
            qp.append_pair("scope", &provider.scopes_string());
            qp.append_pair("state", &state);
            qp.append_pair("code_challenge", &code_challenge);
            qp.append_pair("code_challenge_method", "S256");
            qp.append_pair("nonce", &nonce);
        }

        self.pending_auth.insert(
            state.clone(),
            PendingAuthorization {
                state,
                provider_id: provider.id.clone(),
                nonce,
                code_verifier,
                redirect_uri,
                client_id,
                client_secret,
                token_endpoint_auth_method,
                require_dpop: provider.require_dpop,
                flow,
                created_at: Utc::now(),
            },
        );

        Ok(url.to_string())
    }

    pub(super) fn extract_session_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
        let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
        for pair in cookie_header.split(';') {
            let trimmed = pair.trim();
            if let Some(v) = trimmed.strip_prefix("waddle_session=") {
                return Some(v.to_string());
            }
        }
        None
    }
}
