//! Constrained provider egress client bound to `AdmittedDispatch` +
//! `BrokerCustodyService`, enforcing approved profiles, DNS-pinned SSRF
//! protection, TLS identity, `.no_proxy()`, no redirects, byte/time/concurrency/
//! spend caps, and complete stripping of upstream headers and error bodies.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use url::Url;
use zeroize::Zeroizing;

use crate::admission::service::{AdmittedDispatch, BoundDispatchContext};
use crate::custody::BrokerCustodyService;
use crate::egress::profile::{
    validate_operation_path, EgressBindingPolicy, EgressCaps, EgressProfileRegistry, ProfileError,
    ProviderOperation, ProviderProfile,
};
use crate::egress::ssrf::{resolve_and_validate_addrs, DnsResolver, SsrfError, TokioDnsResolver};
use crate::error::BrokerError;
use crate::secret::SecretBytes;

const DEFAULT_EGRESS_STREAM_CHANNEL_CAPACITY: usize = 16;

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

/// Optional caller-supplied expectation verifying every bound field between the
/// admitted dispatch, the extension invocation context, and the egress request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedEgressBinding<'a> {
    pub trust_domain: &'a str,
    pub extension_id: &'a str,
    pub extension_digest_sha256: [u8; 32],
    pub room_or_space: &'a str,
    pub command_digest_sha256: [u8; 32],
    pub deployment: &'a str,
    pub model: &'a str,
    pub operation: ProviderOperationWire,
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

/// Events yielded progressively from [`BrokerEgressClient::execute_streaming`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressStreamEvent {
    /// A single validated SSE chunk.
    Chunk(EgressChunk),
    /// Clean stream completion at upstream EOF with total validated bytes.
    EndOfStream { total_bytes: usize },
    /// Sanitized terminal error encountered mid-stream.
    Error(EgressError),
}

/// Completed non-streaming egress result (upstream headers discarded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Progressive streaming egress handle backed by a bounded background pump task.
///
/// Dropping `EgressStream` synchronously cancels the upstream pump task, closes
/// the upstream HTTP/TLS socket, and releases the egress concurrency permit.
pub struct EgressStream {
    pub status: u16,
    rx: mpsc::Receiver<EgressStreamEvent>,
    cancel_tx: watch::Sender<bool>,
    canceled: Arc<AtomicBool>,
    total_bytes: usize,
    eof: bool,
    failed: bool,
}

impl EgressStream {
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn cancel_sync(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        let _ = self.cancel_tx.send(true);
    }

    pub async fn next_event(&mut self) -> Option<EgressStreamEvent> {
        self.rx.recv().await
    }

    pub async fn next_chunk(&mut self) -> Result<Option<EgressChunk>, EgressError> {
        if self.eof {
            return Ok(None);
        }
        if self.failed {
            return Err(EgressError::Canceled);
        }
        match self.rx.recv().await {
            Some(EgressStreamEvent::Chunk(chunk)) => {
                self.total_bytes = self.total_bytes.saturating_add(chunk.data.len());
                Ok(Some(chunk))
            }
            Some(EgressStreamEvent::EndOfStream { total_bytes }) => {
                self.eof = true;
                self.total_bytes = total_bytes;
                Ok(None)
            }
            Some(EgressStreamEvent::Error(err)) => {
                self.failed = true;
                self.cancel_sync();
                Err(err)
            }
            None => {
                self.failed = true;
                if self.canceled.load(Ordering::SeqCst) {
                    Err(EgressError::Canceled)
                } else {
                    Err(EgressError::TransportFailed)
                }
            }
        }
    }
}

impl Drop for EgressStream {
    fn drop(&mut self) {
        self.cancel_sync();
    }
}

/// Constrained broker-side provider egress executor.
pub struct BrokerEgressClient {
    registry: EgressProfileRegistry,
    custody: Arc<BrokerCustodyService>,
    resolver: Arc<dyn DnsResolver>,
    semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
    loopback_tls_override: Option<SocketAddr>,
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
            loopback_tls_override: None,
        }
    }

    /// Test-only constructor that keeps production `resolve_and_validate_addrs`
    /// unmodified while routing the validated TCP dial to an explicit loopback
    /// TLS mock server with `pinned_ca_pem` required on the profile.
    #[doc(hidden)]
    pub fn with_loopback_tls_test_override(
        registry: EgressProfileRegistry,
        custody: Arc<BrokerCustodyService>,
        resolver: Arc<dyn DnsResolver>,
        loopback_addr: SocketAddr,
    ) -> Self {
        Self {
            registry,
            custody,
            resolver,
            semaphores: Mutex::new(HashMap::new()),
            loopback_tls_override: Some(loopback_addr),
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
        self.execute_unary_internal(dispatch, req, None).await
    }

    /// Executes a constrained non-streaming inference call while asserting exact
    /// equality against caller-supplied [`ExpectedEgressBinding`].
    pub async fn execute_unary_checked(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
        expected: ExpectedEgressBinding<'_>,
    ) -> Result<EgressResponse, EgressError> {
        self.execute_unary_internal(dispatch, req, Some(expected))
            .await
    }

    /// Executes a constrained streaming inference call bound to an [`AdmittedDispatch`].
    pub async fn execute_streaming(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
    ) -> Result<EgressStream, EgressError> {
        self.execute_streaming_internal(dispatch, req, None).await
    }

    /// Executes a constrained streaming inference call while asserting exact
    /// equality against caller-supplied [`ExpectedEgressBinding`].
    pub async fn execute_streaming_checked(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
        expected: ExpectedEgressBinding<'_>,
    ) -> Result<EgressStream, EgressError> {
        self.execute_streaming_internal(dispatch, req, Some(expected))
            .await
    }

    fn prepare_preflight(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
        expect_streaming: bool,
        expected: Option<ExpectedEgressBinding<'_>>,
    ) -> Result<(ProviderProfile, Vec<u8>), EgressError> {
        if *dispatch.cancel_rx.borrow() {
            return Err(EgressError::Canceled);
        }

        let context = &dispatch.dispatch_context;
        verify_dispatch_matches_request(context, req)?;
        if req.stream != expect_streaming {
            return Err(EgressError::BindingMismatch("stream mode mismatch"));
        }
        if let Some(exp) = expected {
            verify_expected_binding(context, req, exp)?;
        }

        let operation: ProviderOperation = req.operation.into();
        if let Some(requested_path) = req.requested_path.as_deref() {
            validate_operation_path(requested_path, operation)?;
        }

        let (profile_ref, policy_opt) = self.registry.resolve_with_policy(
            &req.provider,
            &req.deployment,
            &req.model,
            operation,
        )?;
        let profile = profile_ref.clone();

        if let Some(policy) = policy_opt {
            verify_binding_policy(context, policy)?;
        }

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

        Ok((profile, body_bytes))
    }

    async fn open_validated_response(
        &self,
        dispatch: &AdmittedDispatch,
        profile: &ProviderProfile,
        body_bytes: Vec<u8>,
        deadline: Instant,
    ) -> Result<reqwest::Response, EgressError> {
        let context = &dispatch.dispatch_context;
        let credential_id = context.payer.credential_key().credential_id;
        let mut cancel_rx = dispatch.cancel_rx.clone();

        let open_fut = async {
            let secret = self
                .custody
                .unseal_for_provider_egress(&context.tenant_id, credential_id, &context.provider)
                .await
                .map_err(sanitize_broker_error)?;

            let target_url = build_target_url(profile)?;
            let host = target_url.host_str().ok_or(SsrfError::MissingHost)?;
            let port = target_url
                .port_or_known_default()
                .ok_or(SsrfError::MissingHost)?;
            let resolved_addrs =
                resolve_and_validate_addrs(self.resolver.as_ref(), host, port).await?;

            let connect_addrs: Vec<SocketAddr> = match self.loopback_tls_override {
                Some(override_addr) => {
                    if !override_addr.ip().is_loopback() {
                        return Err(EgressError::Ssrf(SsrfError::BlockedIpAddress(
                            override_addr.ip(),
                        )));
                    }
                    if profile.pinned_ca_pem.is_none() {
                        return Err(EgressError::BindingMismatch(
                            "loopback TLS test override requires pinned_ca_pem",
                        ));
                    }
                    vec![override_addr]
                }
                None => resolved_addrs,
            };

            let client = build_pinned_client(profile, host, &connect_addrs)?;
            let auth_header = build_bearer_header(&secret)?;
            drop(secret);

            let request = client
                .post(target_url)
                .header(reqwest::header::AUTHORIZATION, &*auth_header)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .body(body_bytes);

            let response = tokio::select! {
                biased;
                changed = cancel_rx.changed() => {
                    let _ = changed;
                    return Err(EgressError::Canceled);
                }
                res = request.send() => {
                    res.map_err(|_| EgressError::TransportFailed)?
                }
            };
            drop(auth_header);

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
            Ok(response)
        };

        tokio::time::timeout_at(deadline, open_fut)
            .await
            .map_err(|_| EgressError::Timeout)?
    }

    async fn execute_unary_internal(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
        expected: Option<ExpectedEgressBinding<'_>>,
    ) -> Result<EgressResponse, EgressError> {
        let (profile, body_bytes) = self.prepare_preflight(dispatch, req, false, expected)?;
        let semaphore = self.semaphore_for_profile(&profile);
        let _permit = semaphore
            .try_acquire_owned()
            .map_err(|_| EgressError::ConcurrencyExhausted)?;
        let deadline = Instant::now() + Duration::from_millis(profile.caps.request_timeout_ms);

        let mut response = self
            .open_validated_response(dispatch, &profile, body_bytes, deadline)
            .await?;
        let status_u16 = response.status().as_u16();
        let mut cancel_rx = dispatch.cancel_rx.clone();

        let body_fut = async {
            let mut full = Vec::new();
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
                let next_len =
                    full.len()
                        .checked_add(chunk.len())
                        .ok_or(EgressError::ResponseTooLarge {
                            max: profile.caps.max_response_bytes,
                        })?;
                if next_len > profile.caps.max_response_bytes {
                    return Err(EgressError::ResponseTooLarge {
                        max: profile.caps.max_response_bytes,
                    });
                }
                full.extend_from_slice(&chunk);
            }
            validate_json_response(&full)?;
            Ok(EgressResponse {
                status: status_u16,
                body: full,
            })
        };

        tokio::time::timeout_at(deadline, body_fut)
            .await
            .map_err(|_| EgressError::Timeout)?
    }

    async fn execute_streaming_internal(
        &self,
        dispatch: &AdmittedDispatch,
        req: &EgressInferenceRequest,
        expected: Option<ExpectedEgressBinding<'_>>,
    ) -> Result<EgressStream, EgressError> {
        let (profile, body_bytes) = self.prepare_preflight(dispatch, req, true, expected)?;
        let semaphore = self.semaphore_for_profile(&profile);
        let permit = semaphore
            .try_acquire_owned()
            .map_err(|_| EgressError::ConcurrencyExhausted)?;
        let deadline = Instant::now() + Duration::from_millis(profile.caps.request_timeout_ms);

        let response = self
            .open_validated_response(dispatch, &profile, body_bytes, deadline)
            .await?;
        let status_u16 = response.status().as_u16();

        let (tx, rx) = mpsc::channel(DEFAULT_EGRESS_STREAM_CHANNEL_CAPACITY);
        let (cancel_tx, consumer_cancel_rx) = watch::channel(false);
        let canceled = Arc::new(AtomicBool::new(false));
        let replica_cancel_rx = dispatch.cancel_rx.clone();

        tokio::spawn(stream_response_pump(
            permit,
            response,
            profile.caps,
            deadline,
            replica_cancel_rx,
            consumer_cancel_rx,
            tx,
        ));

        Ok(EgressStream {
            status: status_u16,
            rx,
            cancel_tx,
            canceled,
            total_bytes: 0,
            eof: false,
            failed: false,
        })
    }
}

async fn send_terminal_event(
    tx: &mpsc::Sender<EgressStreamEvent>,
    event: EgressStreamEvent,
    consumer_cancel_rx: &mut watch::Receiver<bool>,
) {
    if *consumer_cancel_rx.borrow() {
        return;
    }
    tokio::select! {
        biased;
        _ = consumer_cancel_rx.changed() => {}
        _ = tx.send(event) => {}
        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
    }
}

async fn stream_response_pump(
    _permit: OwnedSemaphorePermit,
    mut response: reqwest::Response,
    caps: EgressCaps,
    deadline: Instant,
    mut replica_cancel_rx: watch::Receiver<bool>,
    mut consumer_cancel_rx: watch::Receiver<bool>,
    tx: mpsc::Sender<EgressStreamEvent>,
) {
    let mut total_bytes = 0usize;
    let mut sse_buffer = Vec::new();

    loop {
        if *replica_cancel_rx.borrow() {
            send_terminal_event(
                &tx,
                EgressStreamEvent::Error(EgressError::Canceled),
                &mut consumer_cancel_rx,
            )
            .await;
            break;
        }
        if *consumer_cancel_rx.borrow() {
            break;
        }

        let next = tokio::select! {
            biased;
            changed = replica_cancel_rx.changed() => {
                let _ = changed;
                send_terminal_event(
                    &tx,
                    EgressStreamEvent::Error(EgressError::Canceled),
                    &mut consumer_cancel_rx,
                )
                .await;
                break;
            }
            changed = consumer_cancel_rx.changed() => {
                let _ = changed;
                break;
            }
            _ = tokio::time::sleep_until(deadline) => {
                send_terminal_event(
                    &tx,
                    EgressStreamEvent::Error(EgressError::Timeout),
                    &mut consumer_cancel_rx,
                )
                .await;
                break;
            }
            chunk_res = response.chunk() => {
                match chunk_res {
                    Ok(opt) => opt,
                    Err(_) => {
                        send_terminal_event(
                            &tx,
                            EgressStreamEvent::Error(EgressError::TransportFailed),
                            &mut consumer_cancel_rx,
                        )
                        .await;
                        break;
                    }
                }
            }
        };

        let Some(chunk) = next else {
            if !sse_buffer.is_empty() {
                if let Err(err) = validate_sse_line(&sse_buffer) {
                    send_terminal_event(
                        &tx,
                        EgressStreamEvent::Error(err),
                        &mut consumer_cancel_rx,
                    )
                    .await;
                    break;
                }
            }
            send_terminal_event(
                &tx,
                EgressStreamEvent::EndOfStream { total_bytes },
                &mut consumer_cancel_rx,
            )
            .await;
            break;
        };

        if chunk.len() > caps.max_chunk_bytes {
            send_terminal_event(
                &tx,
                EgressStreamEvent::Error(EgressError::ChunkTooLarge {
                    size: chunk.len(),
                    max: caps.max_chunk_bytes,
                }),
                &mut consumer_cancel_rx,
            )
            .await;
            break;
        }

        let Some(next_total) = total_bytes.checked_add(chunk.len()) else {
            send_terminal_event(
                &tx,
                EgressStreamEvent::Error(EgressError::ResponseTooLarge {
                    max: caps.max_response_bytes,
                }),
                &mut consumer_cancel_rx,
            )
            .await;
            break;
        };
        if next_total > caps.max_response_bytes {
            send_terminal_event(
                &tx,
                EgressStreamEvent::Error(EgressError::ResponseTooLarge {
                    max: caps.max_response_bytes,
                }),
                &mut consumer_cancel_rx,
            )
            .await;
            break;
        }
        total_bytes = next_total;

        sse_buffer.extend_from_slice(&chunk);
        if sse_buffer.len() > caps.max_response_bytes {
            send_terminal_event(
                &tx,
                EgressStreamEvent::Error(EgressError::ResponseTooLarge {
                    max: caps.max_response_bytes,
                }),
                &mut consumer_cancel_rx,
            )
            .await;
            break;
        }
        if let Err(err) = validate_sse_buffer(&mut sse_buffer) {
            send_terminal_event(&tx, EgressStreamEvent::Error(err), &mut consumer_cancel_rx).await;
            break;
        }

        let event = EgressStreamEvent::Chunk(EgressChunk {
            data: chunk.to_vec(),
        });
        let send_ok = tokio::select! {
            biased;
            changed = replica_cancel_rx.changed() => {
                let _ = changed;
                send_terminal_event(
                    &tx,
                    EgressStreamEvent::Error(EgressError::Canceled),
                    &mut consumer_cancel_rx,
                )
                .await;
                false
            }
            changed = consumer_cancel_rx.changed() => {
                let _ = changed;
                false
            }
            _ = tokio::time::sleep_until(deadline) => {
                send_terminal_event(
                    &tx,
                    EgressStreamEvent::Error(EgressError::Timeout),
                    &mut consumer_cancel_rx,
                )
                .await;
                false
            }
            res = tx.send(event) => res.is_ok(),
        };
        if !send_ok {
            break;
        }
    }
}

fn build_bearer_header(secret: &SecretBytes) -> Result<Zeroizing<String>, EgressError> {
    let key_str = std::str::from_utf8(secret.as_bytes())
        .map_err(|_| EgressError::Custody("credential is not valid UTF-8".to_owned()))?;
    Ok(Zeroizing::new(format!("Bearer {key_str}")))
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
        let certs = reqwest::Certificate::from_pem_bundle(ca_pem)
            .map_err(|_| EgressError::TransportFailed)?;
        if certs.is_empty() {
            return Err(EgressError::TransportFailed);
        }
        builder = builder.tls_certs_only(certs);
    }

    builder.build().map_err(|_| EgressError::TransportFailed)
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

fn verify_binding_policy(
    context: &BoundDispatchContext,
    policy: &EgressBindingPolicy,
) -> Result<(), EgressError> {
    if let Some(expected_td) = policy.expected_trust_domain.as_deref() {
        if context.trust_domain != expected_td {
            return Err(EgressError::BindingMismatch(
                "admitted trust_domain does not match profile binding policy",
            ));
        }
    }
    if !policy.allowed_extension_ids.is_empty()
        && !policy
            .allowed_extension_ids
            .iter()
            .any(|id| id == &context.extension_id)
    {
        return Err(EgressError::BindingMismatch(
            "admitted extension_id is not permitted by profile binding policy",
        ));
    }
    if !policy.allowed_extension_digests.is_empty()
        && !policy
            .allowed_extension_digests
            .iter()
            .any(|digest| *digest == context.extension_digest_sha256)
    {
        return Err(EgressError::BindingMismatch(
            "admitted extension_digest_sha256 is not permitted by profile binding policy",
        ));
    }
    Ok(())
}

fn verify_expected_binding(
    dispatch: &BoundDispatchContext,
    req: &EgressInferenceRequest,
    expected: ExpectedEgressBinding<'_>,
) -> Result<(), EgressError> {
    if dispatch.trust_domain != expected.trust_domain {
        return Err(EgressError::BindingMismatch(
            "admitted trust_domain does not match expected binding",
        ));
    }
    if dispatch.extension_id != expected.extension_id {
        return Err(EgressError::BindingMismatch(
            "admitted extension_id does not match expected binding",
        ));
    }
    if dispatch.extension_digest_sha256 != expected.extension_digest_sha256 {
        return Err(EgressError::BindingMismatch(
            "admitted extension_digest_sha256 does not match expected binding",
        ));
    }
    if dispatch.room_or_space.scope_id != expected.room_or_space {
        return Err(EgressError::BindingMismatch(
            "admitted room_or_space does not match expected binding",
        ));
    }
    if dispatch.command_digest_sha256 != expected.command_digest_sha256 {
        return Err(EgressError::BindingMismatch(
            "admitted command_digest_sha256 does not match expected binding",
        ));
    }
    if req.deployment != expected.deployment {
        return Err(EgressError::BindingMismatch(
            "request deployment does not match expected binding",
        ));
    }
    if req.model != expected.model {
        return Err(EgressError::BindingMismatch(
            "request model does not match expected binding",
        ));
    }
    if req.operation != expected.operation {
        return Err(EgressError::BindingMismatch(
            "request operation does not match expected binding",
        ));
    }
    Ok(())
}

/// Validates a bound dispatch context and request payload directly without
/// mutating admission state.
pub fn verify_dispatch_matches_request(
    dispatch: &BoundDispatchContext,
    req: &EgressInferenceRequest,
) -> Result<(), EgressError> {
    if dispatch.provider != req.provider {
        return Err(EgressError::BindingMismatch(
            "request provider does not match admitted dispatch",
        ));
    }
    if dispatch.trust_domain.trim().is_empty() {
        return Err(EgressError::BindingMismatch(
            "bound dispatch trust_domain is empty",
        ));
    }
    if dispatch.extension_id.trim().is_empty() {
        return Err(EgressError::BindingMismatch(
            "bound dispatch extension_id is empty",
        ));
    }
    if dispatch.extension_digest_sha256 == [0u8; 32] {
        return Err(EgressError::BindingMismatch(
            "bound dispatch extension_digest_sha256 is zero",
        ));
    }
    if dispatch.room_or_space.scope_id.trim().is_empty() {
        return Err(EgressError::BindingMismatch(
            "bound dispatch room_or_space is empty",
        ));
    }
    if dispatch.command_digest_sha256 == [0u8; 32] {
        return Err(EgressError::BindingMismatch(
            "bound dispatch command_digest_sha256 is zero",
        ));
    }

    let obj = req
        .body_json
        .as_object()
        .ok_or(EgressError::BindingMismatch(
            "request body_json must be a JSON object",
        ))?;

    if let Some(stream_val) = obj.get("stream") {
        if stream_val != &Value::Bool(req.stream) {
            return Err(EgressError::BindingMismatch(
                "body_json stream field cannot override stream mode",
            ));
        }
    }

    if let Some(model_val) = obj.get("model") {
        if model_val.as_str() != Some(req.model.as_str()) {
            return Err(EgressError::BindingMismatch(
                "body_json model does not match approved request model",
            ));
        }
    }

    for token_key in ["max_tokens", "max_output_tokens"] {
        if let Some(limit_val) = obj.get(token_key) {
            let Some(requested_tokens) = limit_val.as_u64() else {
                return Err(EgressError::BindingMismatch(
                    "body_json max output tokens must be a positive integer",
                ));
            };
            if requested_tokens == 0 || requested_tokens > dispatch.budget.max_output_tokens {
                return Err(EgressError::BindingMismatch(
                    "body_json max output tokens exceeds admitted budget",
                ));
            }
        }
    }

    Ok(())
}
