use crate::auth::redirect::TrustedWebOrigins;
use crate::profile::ProfileSource;
use crate::server::caps_resolution::ServerDomainJid;
use std::sync::OnceLock;

use super::{
    header, oidc, ActorRef, AppState, Arc, AuthError, AuthHandshakeStore, AuthProviderConfig,
    AuthProviderKind, AuthProviderTokenEndpointAuthMethod, BootstrapMembershipConfig, DashMap,
    Digest, Engine, Extension, IdentityService, Mutex, PendingAuthorization, PendingFlow,
    PermissionActor, ProviderRegistry, ResolvedTenant, ServerConfig, SessionManager, Sha256,
    TenantHostResolver, Utc, URL_SAFE_NO_PAD,
};

/// Typed callback the OIDC callback handler fires after a successful
/// login to materialize a conformant PEP avatar + vCard set. The hook
/// implementation registers the publish chain with the
/// `profile_publish_tracker` (`tokio_util::task::TaskTracker`)
/// internally so login latency is unaffected AND the
/// graceful-shutdown drain can `wait()` on in-flight publishes
/// before tearing down the runtime.
pub type ProfilePublishHook = Arc<dyn Fn(jid::BareJid, ProfileSource) + Send + Sync + 'static>;

/// Shared auth state.
pub struct AuthState {
    pub session_manager: SessionManager,
    pub identity_service: IdentityService,
    /// Providers from `WADDLE_AUTH_PROVIDERS_JSON`. Used only in
    /// single-tenant mode; multi-tenant requests resolve providers from
    /// their tenant manifest via [`Self::providers_for`].
    pub single_tenant_providers: Arc<ProviderRegistry>,
    pub base_url: String,
    /// Where a browser sign-in may return to; see
    /// [`crate::auth::redirect::TrustedWebOrigins`].
    pub trusted_web_origins: TrustedWebOrigins,
    /// Trusted public RFC 7395 endpoint advertised to XMPP clients. Kept
    /// separate from `base_url`, which describes the HTTP application and may
    /// have a different external scheme/authority behind a TLS terminator.
    pub public_xmpp_websocket_url: url::Url,
    pub xmpp_domain: String,
    /// Pre-parsed XMPP domain JID for server-issued IQ stanzas
    /// (e.g. XEP-0115 caps disco#info queries). Validated at startup
    /// so downstream construction sites cannot panic on a malformed
    /// `WADDLE_XMPP_DOMAIN` value at runtime.
    pub caps_server_domain: ServerDomainJid,
    pub http_client: reqwest::Client,
    /// Durable OIDC/OAuth handshake state (`pending_auth`,
    /// `device_auth`, `xmpp_auth_codes`) shared across replicas via the
    /// global database — see #1336. Never cache these entries in
    /// process memory: the pod that mints a state/code is not
    /// necessarily the pod that redeems it.
    pub auth_handshake: AuthHandshakeStore,
    pub dynamic_oidc_clients: Arc<DashMap<String, oidc::DynamicClientRegistration>>,
    pub dynamic_oidc_client_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    pub permission_actor: ActorRef<PermissionActor>,
    pub bootstrap_membership: BootstrapMembershipConfig,
    pub tenant_resolver: Arc<TenantHostResolver>,
    /// Set once at startup after `WebSocketState` exists. The callback
    /// handler reads it via `profile_publish_hook()` and spawns it on
    /// every successful OIDC login.
    profile_publish_hook: OnceLock<ProfilePublishHook>,
}

impl AuthState {
    pub fn new(
        app_state: Arc<AppState>,
        server_config: &ServerConfig,
        public_xmpp_websocket_url: &url::Url,
        encryption_key: Option<&[u8]>,
    ) -> Self {
        let session_manager =
            SessionManager::new(app_state.db_pool.global_actor().clone(), encryption_key);
        let identity_service = IdentityService::new(app_state.db_pool.global_actor().clone());
        let single_tenant_providers = Arc::new(
            ProviderRegistry::try_new(server_config.auth.providers.clone())
                .unwrap_or_else(|e| panic!("invalid provider config at startup: {}", e)),
        );

        let xmpp_domain =
            std::env::var("WADDLE_XMPP_DOMAIN").unwrap_or_else(|_| "localhost".to_string());
        let caps_server_domain = ServerDomainJid::parse(&xmpp_domain).unwrap_or_else(|error| {
            panic!("WADDLE_XMPP_DOMAIN={xmpp_domain:?} is not a valid JID at startup: {error}")
        });
        let bootstrap_membership = BootstrapMembershipConfig::from_env().serving(&xmpp_domain);
        let base_url = server_config.base_url.trim_end_matches('/').to_string();
        let tenant_resolver = if app_state.tenant_resolver.is_multi_tenant() {
            app_state.tenant_resolver.clone()
        } else {
            let domain_part: jid::DomainPart = xmpp_domain
                .parse()
                .unwrap_or_else(|_| "localhost".parse().expect("valid localhost domain"));
            let muc_domain: jid::DomainPart = format!("muc.{}", domain_part.as_str())
                .parse()
                .unwrap_or_else(|_| "muc.localhost".parse().expect("valid muc domain"));
            let spaces_jid: jid::BareJid = format!("spaces.{}", domain_part.as_str())
                .parse()
                .unwrap_or_else(|_| "spaces.localhost".parse().expect("valid spaces jid"));
            let tenant_id = waddle_broker_core::identity::TenantId::parse(domain_part.as_str())
                .unwrap_or_else(|_| app_state.tenant_resolver.fallback_tenant().tenant_id);
            match ResolvedTenant::from_static_defaults(
                tenant_id,
                &base_url,
                public_xmpp_websocket_url,
                domain_part,
                muc_domain,
                spaces_jid,
                bootstrap_membership.owner_localparts().to_vec(),
            ) {
                Ok(fallback) => Arc::new(TenantHostResolver::single_tenant(fallback)),
                Err(_) => app_state.tenant_resolver.clone(),
            }
        };
        Self {
            session_manager,
            identity_service,
            single_tenant_providers,
            base_url,
            trusted_web_origins: server_config.trusted_web_origins.clone(),
            public_xmpp_websocket_url: public_xmpp_websocket_url.clone(),
            xmpp_domain,
            caps_server_domain,
            http_client: reqwest::Client::new(),
            auth_handshake: AuthHandshakeStore::new(app_state.db_pool.global_actor().clone()),
            dynamic_oidc_clients: Arc::new(DashMap::new()),
            dynamic_oidc_client_locks: Arc::new(DashMap::new()),
            permission_actor: app_state.permission_actor.clone(),
            bootstrap_membership,
            tenant_resolver,
            profile_publish_hook: OnceLock::new(),
        }
    }

    /// Resolve the request's tenant from the middleware extension, or fall back
    /// to this server's static single-tenant configuration (used by direct unit
    /// tests that call auth routers without the outer host-routing middleware).
    pub fn tenant_or_fallback(&self, ext: Option<Extension<ResolvedTenant>>) -> ResolvedTenant {
        match ext {
            Some(Extension(tenant)) => tenant,
            None => self.tenant_resolver.fallback_tenant(),
        }
    }

    /// Sign-in providers for `tenant`. In multi-tenant mode these come only
    /// from the tenant's manifest (none declared means sign-in is disabled);
    /// in single-tenant mode they come from `WADDLE_AUTH_PROVIDERS_JSON`.
    pub fn providers_for(&self, tenant: &ResolvedTenant) -> Arc<ProviderRegistry> {
        if !self.tenant_resolver.is_multi_tenant() {
            return Arc::clone(&self.single_tenant_providers);
        }
        self.tenant_resolver
            .snapshot()
            .ok()
            .and_then(|table| table.auth_providers(&tenant.tenant_id))
            .unwrap_or_default()
    }

    /// Returns `true` when `user_jid` belongs to `tenant` in multi-tenant mode
    /// (enforcing strict cross-tenant credential isolation), or unconditionally
    /// in single-tenant mode where only one deployment exists.
    pub(crate) fn session_matches_tenant(&self, tenant: &ResolvedTenant, user_jid: &str) -> bool {
        !self.tenant_resolver.is_multi_tenant() || tenant.owns_jid_str(user_jid)
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
        // Keyed by issuer too: a manifest reload may point the same provider
        // id at a different issuer, whose clients are not interchangeable.
        let cache_key = format!(
            "{}\0{}\0{}",
            provider.id,
            provider.issuer.as_deref().unwrap_or_default(),
            redirect_uri
        );
        if let Some(cached) = self.dynamic_oidc_clients.get(&cache_key) {
            return Ok(cached.clone());
        }

        let lock = self
            .dynamic_oidc_client_locks
            .entry(cache_key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        if let Some(cached) = self.dynamic_oidc_clients.get(&cache_key) {
            return Ok(cached.clone());
        }

        let registered =
            oidc::register_dynamic_client(&self.http_client, provider, discovery, redirect_uri)
                .await?;
        self.dynamic_oidc_clients
            .insert(cache_key, registered.clone());
        Ok(registered)
    }

    #[cfg(test)]
    pub(super) fn session_cookie_header(&self, session_id: Option<&str>, max_age: i64) -> String {
        let tenant = self.tenant_resolver.fallback_tenant();
        self.session_cookie_header_for_tenant(&tenant, session_id, max_age)
    }

    pub(super) fn session_cookie_header_for_tenant(
        &self,
        tenant: &ResolvedTenant,
        session_id: Option<&str>,
        max_age: i64,
    ) -> String {
        let mut parts = vec![
            format!("waddle_session={}", session_id.unwrap_or_default()),
            "Path=/".to_string(),
            "HttpOnly".to_string(),
            "SameSite=Lax".to_string(),
            format!("Max-Age={max_age}"),
        ];

        if tenant.is_https() {
            parts.push("Secure".to_string());
        }

        parts.join("; ")
    }

    fn create_pkce_verifier() -> String {
        use rand::RngExt;
        let bytes: [u8; 32] = rand::rng().random();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn pkce_challenge(verifier: &str) -> String {
        let digest = Sha256::digest(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }

    fn random_state() -> String {
        use rand::RngExt;
        let bytes: [u8; 24] = rand::rng().random();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub async fn start_authorization(
        &self,
        provider: &AuthProviderConfig,
        flow: PendingFlow,
    ) -> Result<String, AuthError> {
        let tenant = self.tenant_resolver.fallback_tenant();
        self.start_authorization_for_tenant(provider, flow, &tenant)
            .await
    }

    pub async fn start_authorization_for_tenant(
        &self,
        provider: &AuthProviderConfig,
        flow: PendingFlow,
        tenant: &ResolvedTenant,
    ) -> Result<String, AuthError> {
        let state = Self::random_state();
        let nonce = Self::random_state();
        let code_verifier = Self::create_pkce_verifier();
        let code_challenge = Self::pkce_challenge(&code_verifier);
        let redirect_uri = tenant.callback_url();
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

        self.auth_handshake
            .insert_pending(&PendingAuthorization {
                state,
                tenant_id: tenant.tenant_id.clone(),
                provider_id: provider.id.clone(),
                issuer: provider.issuer.clone(),
                nonce,
                code_verifier,
                redirect_uri,
                client_id,
                client_secret,
                token_endpoint_auth_method,
                require_dpop: provider.require_dpop,
                flow,
                created_at: Utc::now(),
            })
            .await?;

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
