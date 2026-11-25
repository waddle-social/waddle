//! Constrained provider egress client bound to `AdmittedDispatch` +
//! `BrokerCustodyService`, enforcing approved profiles, DNS-pinned SSRF
//! protection, TLS identity, `.no_proxy()`, no redirects, byte/time/concurrency/
//! spend caps, and complete stripping of upstream headers and error bodies.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{watch, Semaphore};
use url::Url;

use crate::admission::service::{AdmittedDispatch, BoundDispatchContext};
use crate::custody::BrokerCustodyService;
use crate::egress::profile::{
    validate_operation_path, EgressProfileRegistry, ProfileError, ProviderOperation,
    ProviderProfile,
};
use crate::egress::ssrf::{resolve_and_validate_addrs, DnsResolver, SsrfError, TokioDnsResolver};
use crate::error::BrokerError;
use crate::secret::SecretBytes;

/// Sanitized egress error returned to cells and guests.
///
/// Deliberately NEVER carries upstream HTTP response headers, upstream error
/// response bodies, raw URLs, or provider credentials.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EgressError {
    #[error("profile rejected: {0}")]
    Profile(#[from] ProfileError),
    #[error("SSRF policy rejected target: {0}")]
    Ssrf(#[from] SsrfError),
    #[error("dispatch binding mismatch: {0}")]
    BindingMismatch(&'static str),
    #[error("credential custody error: {0}")]
    Custody(String),
    #[error("request payload exceeds byte limit ({size} > {max})")]
    RequestTooLarge { size: usize, max: usize },
    #[error("response payload exceeds byte limit ({max} bytes)")]
    ResponseTooLarge { max: usize },
    #[error("stream chunk exceeds byte limit ({size} > {max})")]
    ChunkTooLarge { size: usize, max: usize },
    #[error("reserved spend ({reserved}) exceeds profile spend cap ({cap})")]
    SpendCapExceeded { reserved: u64, cap: u64 },
    #[error("egress concurrency capacity exhausted")]
    ConcurrencyExhausted,
    #[error("inference stream canceled by replica fence")]
    Canceled,
    #[error("provider request timed out")]
    Timeout,
    #[error("upstream redirect (HTTP {status}) blocked")]
    RedirectBlocked { status: u16 },
    #[error("upstream returned HTTP {status} (upstream headers and error body discarded)")]
    UpstreamStatus { status: u16 },
    #[error("upstream response failed shape validation")]
    MalformedUpstreamResponse,
    #[error("TLS or HTTP client transport failure")]
    TransportFailed,
}

/// Typed inference request submitted by a cell/guest after broker admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressInferenceRequest {
    pub provider: String,
    pub deployment: String,
    pub model: String,
    pub operation: ProviderOperationWire,
    /// Must remain `None` or equal the exact approved profile operation path.
    /// Any admin path or mismatch is rejected unconditionally.
    pub requested_path: Option<String>,
    pub body_json: Value,
    pub stream: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOperationWire {
    ChatCompletions,
    Responses,
    Messages,
}

impl From<ProviderOperationWire> for ProviderOperation {
    fn from(value: ProviderOperationWire) -> Self {
        match value {
            ProviderOperationWire::ChatCompletions => Self::ChatCompletions,
            ProviderOperationWire::Responses => Self::Responses,
            ProviderOperationWire::Messages => Self::Messages,
        }
    }
}

/// Validated response chunk or body returned from constrained provider egress.
///
/// Upstream headers are intentionally omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressChunk {
    pub data: Vec<u8>,
}

/// Completed non-streaming egress result (upstream headers discarded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Streaming egress result yielding bounded validated chunks.
pub struct EgressStream {
    pub status: u16,
    pub chunks: Vec<EgressChunk>,
    pub total_bytes: usize,
}

/// Constrained broker-side provider egress executor.
pub struct BrokerEgressClient {
    registry: EgressProfileRegistry,
    custody: Arc<BrokerCustodyService>,
    resolver: Arc<dyn DnsResolver>,
    semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl BrokerEgressClient {
    pub fn new(registry: EgressProfileRegistry, custody: Arc<BrokerCustodyService>) -> Self {
        Self::with_resolver(registry, custody, Arc::new(TokioDnsResolver))
    }

    pub fn with_resolver(
        registry: EgressProfileRegistry,
        custody: Arc<BrokerCustodyService>,
        resolver: Arc<dyn DnsResolver>,
    ) -> Self {
        Self {
            registry,
            custody,
            resolver,
            semaphores: Mutex::new(HashMap::new()),
        }
    }

    fn semaphore_for_profile(&self, profile: &ProviderProfile) -> Arc<Semaphore> {
        let key = format!(
            "{}:{}:{}:{}",
            profile.provider,
            profile.deployment,
            profile.model,
            profile.operation.as_str()
        );
        let mut guard = self
            .semaphores
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .entry(key)
            .or_insert_with(|| Arc::new(Semaphore::new(profile.caps.max_concurrent_requests)))
            .clone()
    }

    /// Executes a constrained non-streaming inference call bound to an [`AdmittedDispatch`].
    pub async fn execute_unary(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
    ) -> Result<EgressResponse, EgressError> {
        let mut stream = self.execute_internal(dispatch, req, false).await?;
        let mut body = Vec::with_capacity(stream.total_bytes);
        for chunk in stream.chunks.drain(..) {
            body.extend_from_slice(&chunk.data);
        }
        Ok(EgressResponse {
            status: stream.status,
            body,
        })
    }

    /// Executes a constrained streaming inference call bound to an [`AdmittedDispatch`].
    pub async fn execute_streaming(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
    ) -> Result<EgressStream, EgressError> {
        self.execute_internal(dispatch, req, true).await
    }

    async fn execute_internal(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
        expect_streaming: bool,
    ) -> Result<EgressStream, EgressError> {
        if *dispatch.cancel_rx.borrow() {
            return Err(EgressError::Canceled);
        }

        let context = &dispatch.dispatch_context;
        if req.provider != context.provider {
            return Err(EgressError::BindingMismatch(
                "request provider does not match admitted dispatch",
            ));
        }
        if req.stream != expect_streaming {
            return Err(EgressError::BindingMismatch("stream mode mismatch"));
        }

        let operation: ProviderOperation = req.operation.into();
        if let Some(requested_path) = req.requested_path.as_deref() {
            validate_operation_path(requested_path, operation)?;
        }

        let profile = self
            .registry
            .resolve(&req.provider, &req.deployment, &req.model, operation)?
            .clone();

        if let Some(requested_path) = req.requested_path.as_deref() {
            if requested_path != profile.operation_path {
                return Err(EgressError::Profile(ProfileError::InvalidPath(
                    "requested_path does not match approved profile operation_path",
                )));
            }
        }

        if context.reserved_spend_microcents > profile.caps.max_spend_microcents {
            return Err(EgressError::SpendCapExceeded {
                reserved: context.reserved_spend_microcents,
                cap: profile.caps.max_spend_microcents,
            });
        }
        if context.budget.max_spend_microcents > profile.caps.max_spend_microcents {
            return Err(EgressError::SpendCapExceeded {
                reserved: context.budget.max_spend_microcents,
                cap: profile.caps.max_spend_microcents,
            });
        }

        let body_bytes = serde_json::to_vec(&req.body_json)
            .map_err(|_| EgressError::BindingMismatch("request body must serialize as JSON"))?;
        if body_bytes.len() > profile.caps.max_request_bytes {
            return Err(EgressError::RequestTooLarge {
                size: body_bytes.len(),
                max: profile.caps.max_request_bytes,
            });
        }

        let semaphore = self.semaphore_for_profile(&profile);
        let _permit = semaphore
            .try_acquire_owned()
            .map_err(|_| EgressError::ConcurrencyExhausted)?;

        let credential_id = context.payer.credential_key().credential_id;
        let secret = self
            .custody
            .unseal_for_provider_egress(&context.tenant_id, credential_id, &context.provider)
            .await
            .map_err(sanitize_broker_error)?;

        let target_url = build_target_url(&profile)?;
        let host = target_url.host_str().ok_or(SsrfError::MissingHost)?;
        let port = target_url
            .port_or_known_default()
            .ok_or(SsrfError::MissingHost)?;
        let resolved_addrs = resolve_and_validate_addrs(self.resolver.as_ref(), host, port).await?;

        let client = build_pinned_client(&profile, host, &resolved_addrs)?;
        let mut cancel_rx = dispatch.cancel_rx.clone();
        let timeout_dur = Duration::from_millis(profile.caps.request_timeout_ms);

        let send_fut = send_and_validate_response(
            &client,
            target_url,
            &profile,
            &secret,
            body_bytes,
            expect_streaming,
            &mut cancel_rx,
        );

        tokio::time::timeout(timeout_dur, send_fut)
            .await
            .map_err(|_| EgressError::Timeout)?
    }
}

fn sanitize_broker_error(err: BrokerError) -> EgressError {
    match err {
        BrokerError::EgressClosedPendingRestore => {
            EgressError::Custody("egress closed pending restore".to_owned())
        }
        BrokerError::CredentialDestroyed(_) => {
            EgressError::Custody("credential destroyed".to_owned())
        }
        BrokerError::CredentialNotFound(_) => {
            EgressError::Custody("credential not found".to_owned())
        }
        BrokerError::AccountRetired(_) => EgressError::Custody("account retired".to_owned()),
        BrokerError::TenantDestroyed(_) => EgressError::Custody("tenant destroyed".to_owned()),
        BrokerError::ProviderMismatch { .. } => {
            EgressError::Custody("credential provider mismatch".to_owned())
        }
        _ => EgressError::Custody("credential custody failure".to_owned()),
    }
}

fn build_target_url(profile: &ProviderProfile) -> Result<Url, EgressError> {
    let joined = format!("{}{}", profile.base_origin, profile.operation_path);
    let parsed = Url::parse(&joined).map_err(|_| ProfileError::InvalidOrigin)?;
    if parsed.scheme() != "https" {
        return Err(ProfileError::InvalidOrigin.into());
    }
    Ok(parsed)
}

fn build_pinned_client(
    profile: &ProviderProfile,
    host: &str,
    resolved_addrs: &[SocketAddr],
) -> Result<reqwest::Client, EgressError> {
    let mut builder = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, resolved_addrs)
        .min_tls_version(reqwest::tls::Version::TLS_1_2)
        .connect_timeout(Duration::from_millis(
            profile.caps.request_timeout_ms.min(10_000),
        ))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd();

    if let Some(ca_pem) = &profile.pinned_ca_pem {
        let cert =
            reqwest::Certificate::from_pem(ca_pem).map_err(|_| EgressError::TransportFailed)?;
        builder = builder.add_root_certificate(cert);
    }

    builder.build().map_err(|_| EgressError::TransportFailed)
}

async fn send_and_validate_response(
    client: &reqwest::Client,
    target_url: Url,
    profile: &ProviderProfile,
    secret: &SecretBytes,
    body_bytes: Vec<u8>,
    expect_streaming: bool,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<EgressStream, EgressError> {
    let auth_header = {
        let key_str = std::str::from_utf8(secret.as_bytes())
            .map_err(|_| EgressError::Custody("credential is not valid UTF-8".to_owned()))?;
        format!("Bearer {key_str}")
    };

    let request = client
        .post(target_url)
        .header(reqwest::header::AUTHORIZATION, auth_header)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .body(body_bytes);

    let mut response = tokio::select! {
        biased;
        changed = cancel_rx.changed() => {
            if changed.is_ok() && *cancel_rx.borrow() {
                return Err(EgressError::Canceled);
            }
            return Err(EgressError::Canceled);
        }
        res = request.send() => {
            res.map_err(|_| EgressError::TransportFailed)?
        }
    };

    let status = response.status();
    let status_u16 = status.as_u16();
    // Explicitly discard all response headers (never inspect/forward/echo).
    if status.is_redirection() {
        return Err(EgressError::RedirectBlocked { status: status_u16 });
    }
    if !status.is_success() {
        // Deliberately drop response without reading or surfacing upstream error body/headers.
        return Err(EgressError::UpstreamStatus { status: status_u16 });
    }

    let mut chunks = Vec::new();
    let mut total_bytes = 0usize;
    let mut sse_buffer = Vec::new();

    loop {
        if *cancel_rx.borrow() {
            return Err(EgressError::Canceled);
        }
        let next = tokio::select! {
            biased;
            changed = cancel_rx.changed() => {
                let _ = changed;
                return Err(EgressError::Canceled);
            }
            chunk_res = response.chunk() => {
                chunk_res.map_err(|_| EgressError::TransportFailed)?
            }
        };
        let Some(chunk) = next else {
            break;
        };
        if chunk.len() > profile.caps.max_chunk_bytes {
            return Err(EgressError::ChunkTooLarge {
                size: chunk.len(),
                max: profile.caps.max_chunk_bytes,
            });
        }
        total_bytes =
            total_bytes
                .checked_add(chunk.len())
                .ok_or(EgressError::ResponseTooLarge {
                    max: profile.caps.max_response_bytes,
                })?;
        if total_bytes > profile.caps.max_response_bytes {
            return Err(EgressError::ResponseTooLarge {
                max: profile.caps.max_response_bytes,
            });
        }
        if expect_streaming {
            sse_buffer.extend_from_slice(&chunk);
            validate_sse_buffer(&mut sse_buffer)?;
        }
        chunks.push(EgressChunk {
            data: chunk.to_vec(),
        });
    }

    if expect_streaming {
        if !sse_buffer.is_empty() {
            validate_sse_line(&sse_buffer)?;
        }
    } else {
        let mut full = Vec::with_capacity(total_bytes);
        for c in &chunks {
            full.extend_from_slice(&c.data);
        }
        validate_json_response(&full)?;
    }

    Ok(EgressStream {
        status: status_u16,
        chunks,
        total_bytes,
    })
}

fn validate_json_response(bytes: &[u8]) -> Result<(), EgressError> {
    let val: Value =
        serde_json::from_slice(bytes).map_err(|_| EgressError::MalformedUpstreamResponse)?;
    if !val.is_object() {
        return Err(EgressError::MalformedUpstreamResponse);
    }
    Ok(())
}

fn validate_sse_buffer(buf: &mut Vec<u8>) -> Result<(), EgressError> {
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=pos).collect();
        validate_sse_line(&line)?;
    }
    Ok(())
}

fn validate_sse_line(raw: &[u8]) -> Result<(), EgressError> {
    let text = std::str::from_utf8(raw).map_err(|_| EgressError::MalformedUpstreamResponse)?;
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') {
        return Ok(());
    }
    if let Some(data) = trimmed.strip_prefix("data:") {
        let payload = data.trim();
        if payload == "[DONE]" {
            return Ok(());
        }
        let val: Value =
            serde_json::from_str(payload).map_err(|_| EgressError::MalformedUpstreamResponse)?;
        if !val.is_object() {
            return Err(EgressError::MalformedUpstreamResponse);
        }
        return Ok(());
    }
    if trimmed.starts_with("event:") || trimmed.starts_with("id:") {
        return Ok(());
    }
    Err(EgressError::MalformedUpstreamResponse)
}

/// Validates a bound dispatch context directly without mutating admission state.
pub fn verify_dispatch_matches_request(
    dispatch: &BoundDispatchContext,
    req: &EgressInferenceRequest,
) -> Result<(), EgressError> {
    if dispatch.provider != req.provider {
        return Err(EgressError::BindingMismatch(
            "request provider does not match admitted dispatch",
        ));
    }
    Ok(())
}
