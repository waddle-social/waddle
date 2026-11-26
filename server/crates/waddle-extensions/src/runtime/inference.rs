//! Host-side `InferenceStream` resource and brokered streaming inference trait.
//!
//! Enforces:
//! - Synchronous `Drop` on [`InferenceStream`] triggers an idempotent,
//!   non-blocking upstream cancellation signal even when `tokio::time::timeout`
//!   or an epoch trap drops `Store<HostState>` without invoking async WIT `drop`.
//! - Bounded chunk byte limits (`0`-byte allowed or bounded, `> max_chunk_bytes`
//!   rejected with `ChunkLimitExceeded`) and bounded queue capacity.
//! - Host-verified `terminal_usage`: returns `None` until the host marks the
//!   stream completed at EOF, and never trusts guest-reported usage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch, Mutex};

/// Default maximum chunk byte size accepted from an upstream inference stream.
pub const DEFAULT_MAX_STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Default bounded capacity for host-to-guest chunk buffering.
pub const DEFAULT_STREAM_CHANNEL_CAPACITY: usize = 16;

/// Typed credential reference passed by inference guests in WIT `@4.0.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceCredentialRef {
    /// Use the tenant's enrolled credential under the named logical alias.
    Tenant(String),
    /// Use the invoking user's enrolled credential for the named provider.
    InvokingUser(String),
}

/// One message in a typed inference request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInferenceMessage {
    /// Chat role (`system`, `user`, `assistant`, etc.).
    pub role: String,
    /// Message text content.
    pub content: String,
}

/// Typed host-side inference stream request converted from WIT `@4.0.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInferenceRequest {
    /// Credential reference (`tenant` or `invoking-user`).
    pub credential: InferenceCredentialRef,
    /// Approved provider identifier.
    pub provider: String,
    /// Approved deployment identifier.
    pub deployment: String,
    /// Approved model identifier.
    pub model: String,
    /// Conversation messages to submit to the broker.
    pub messages: Vec<HostInferenceMessage>,
    /// Optional output token ceiling.
    pub max_output_tokens: Option<u32>,
}

/// One bounded chunk yielded from the brokered inference stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInferenceChunk {
    /// Raw UTF-8 delta bytes produced by the provider stream.
    pub delta: Vec<u8>,
    /// Monotonic zero-based sequence number for ordering verification.
    pub sequence: u64,
    /// Optional provider finish reason on the final delta chunk.
    pub finish_reason: Option<String>,
}

/// Host-verified terminal accounting populated after stream EOF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInferenceUsage {
    /// Provider that served the stream.
    pub provider: String,
    /// Model that served the stream.
    pub model: String,
    /// Host-verified input token count.
    pub input_tokens: u64,
    /// Host-verified output token count.
    pub output_tokens: u64,
    /// Host-verified cost in microcents.
    pub cost_microcents: u64,
}

/// Typed inference error codes surfaced to WIT guests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostInferenceErrorCode {
    /// Authorization, admission, or policy denial.
    Denied,
    /// Request failed validation before provider dispatch.
    InvalidRequest,
    /// Stream was canceled by replica fence, timeout, or drop.
    Canceled,
    /// Upstream rate limit or concurrency cap reached.
    RateLimited,
    /// Transient upstream or transport failure.
    TemporaryFailure,
    /// Upstream chunk exceeded `max_chunk_bytes`.
    ChunkLimitExceeded,
}

/// Sanitized host inference error (never carries upstream headers/body/keys).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInferenceError {
    /// Machine-readable error category.
    pub code: HostInferenceErrorCode,
    /// Sanitized host diagnostic message.
    pub message: String,
}

impl HostInferenceError {
    /// Construct a sanitized [`HostInferenceError`].
    #[must_use]
    pub fn new(code: HostInferenceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Events emitted over the bounded upstream stream channel to [`InferenceStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamItem {
    /// A single validated stream chunk.
    Chunk(HostInferenceChunk),
    /// End-of-stream marker carrying host-verified usage accounting.
    EndOfStream(VerifiedInferenceUsage),
    /// Sanitized terminal error from the upstream stream.
    Error(HostInferenceError),
}

/// Non-blocking cancellation handle triggered synchronously on `Drop`.
#[derive(Debug, Clone)]
pub struct StreamCancelHandle {
    canceled: Arc<AtomicBool>,
    cancel_tx: watch::Sender<bool>,
}

impl StreamCancelHandle {
    /// Creates a new cancellation handle and its watch receiver.
    #[must_use]
    pub fn new() -> (Self, watch::Receiver<bool>) {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        (
            Self {
                canceled: Arc::new(AtomicBool::new(false)),
                cancel_tx,
            },
            cancel_rx,
        )
    }

    /// Synchronously and idempotently signals upstream cancellation without
    /// blocking or awaiting. Safe to call inside `Drop`.
    pub fn cancel_sync(&self) {
        if !self.canceled.swap(true, Ordering::SeqCst) {
            let _ = self.cancel_tx.send(true);
        }
    }

    /// Returns `true` if cancellation has already been signaled.
    #[must_use]
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }
}

impl Default for StreamCancelHandle {
    fn default() -> Self {
        Self::new().0
    }
}

/// Opened backend inference stream handed to the host `ResourceTable`.
pub struct OpenedInferenceStream {
    /// Bounded receiver carrying validated chunks and terminal status.
    pub rx: mpsc::Receiver<StreamItem>,
    /// Synchronous cancellation handle triggered on `Drop`.
    pub cancel: StreamCancelHandle,
    /// Maximum allowed byte length for each chunk.
    pub max_chunk_bytes: usize,
}

/// Backend provider/broker streaming adapter injected into [`HostState`](super::HostState).
#[async_trait]
pub trait ExtensionInferenceHost: Send + Sync + 'static {
    /// Open a brokered, admitted inference stream for the given invocation context.
    async fn start_stream(
        &self,
        context: &crate::host_tools::InvocationContext,
        request: HostInferenceRequest,
    ) -> Result<OpenedInferenceStream, HostInferenceError>;
}

/// Default inference host rejecting all inference calls when no broker adapter is configured.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyingExtensionInferenceHost;

#[async_trait]
impl ExtensionInferenceHost for DenyingExtensionInferenceHost {
    async fn start_stream(
        &self,
        _context: &crate::host_tools::InvocationContext,
        _request: HostInferenceRequest,
    ) -> Result<OpenedInferenceStream, HostInferenceError> {
        Err(HostInferenceError::new(
            HostInferenceErrorCode::Denied,
            "brokered inference stream host is not configured",
        ))
    }
}

/// Host-table resource backing WIT `waddle:extension/inference@4.0.0.inference-stream`.
///
/// Dropping this resource (either via explicit WIT `drop` or via `Store<HostState>`
/// drop on `tokio::time::timeout` / epoch trap) synchronously calls
/// `cancel.cancel_sync()` so upstream provider connections abort promptly.
pub struct InferenceStream {
    rx: Arc<Mutex<mpsc::Receiver<StreamItem>>>,
    cancel: StreamCancelHandle,
    max_chunk_bytes: usize,
    completed_usage: Option<VerifiedInferenceUsage>,
    eof: bool,
    failed: bool,
}

impl InferenceStream {
    /// Wrap an [`OpenedInferenceStream`] in a `ResourceTable`-owned [`InferenceStream`].
    #[must_use]
    pub fn from_opened(opened: OpenedInferenceStream) -> Self {
        Self {
            rx: Arc::new(Mutex::new(opened.rx)),
            cancel: opened.cancel,
            max_chunk_bytes: opened.max_chunk_bytes,
            completed_usage: None,
            eof: false,
            failed: false,
        }
    }

    /// Creates a bounded channel-backed [`InferenceStream`] directly for unit/integration tests.
    #[must_use]
    pub fn with_bounded_channel(
        capacity: usize,
        max_chunk_bytes: usize,
    ) -> (
        Self,
        mpsc::Sender<StreamItem>,
        watch::Receiver<bool>,
        StreamCancelHandle,
    ) {
        let cap = capacity.max(1);
        let (tx, rx) = mpsc::channel(cap);
        let (cancel, cancel_rx) = StreamCancelHandle::new();
        let stream = Self {
            rx: Arc::new(Mutex::new(rx)),
            cancel: cancel.clone(),
            max_chunk_bytes,
            completed_usage: None,
            eof: false,
            failed: false,
        };
        (stream, tx, cancel_rx, cancel)
    }

    /// Reads the next validated chunk from the bounded channel.
    ///
    /// Enforces `chunk.delta.len() <= max_chunk_bytes` and stores host-verified
    /// `terminal_usage` only once EOF is observed.
    ///
    /// # Errors
    ///
    /// Returns [`HostInferenceError`] if the stream was canceled, exceeded
    /// `max_chunk_bytes`, or ended with a sanitized upstream error.
    pub async fn next_chunk(&mut self) -> Result<Option<HostInferenceChunk>, HostInferenceError> {
        if self.eof {
            return Ok(None);
        }
        if self.failed {
            return Err(HostInferenceError::new(
                HostInferenceErrorCode::Canceled,
                "inference stream already terminated",
            ));
        }
        if self.cancel.is_canceled() {
            self.failed = true;
            return Err(HostInferenceError::new(
                HostInferenceErrorCode::Canceled,
                "inference stream canceled",
            ));
        }

        let item = {
            let mut rx = self.rx.lock().await;
            rx.recv().await
        };

        match item {
            Some(StreamItem::Chunk(chunk)) => {
                if chunk.delta.len() > self.max_chunk_bytes {
                    self.failed = true;
                    self.cancel.cancel_sync();
                    return Err(HostInferenceError::new(
                        HostInferenceErrorCode::ChunkLimitExceeded,
                        format!(
                            "stream chunk exceeded max_chunk_bytes ({} > {})",
                            chunk.delta.len(),
                            self.max_chunk_bytes
                        ),
                    ));
                }
                Ok(Some(chunk))
            }
            Some(StreamItem::EndOfStream(usage)) => {
                self.eof = true;
                self.completed_usage = Some(usage);
                Ok(None)
            }
            Some(StreamItem::Error(err)) => {
                self.failed = true;
                self.cancel.cancel_sync();
                Err(err)
            }
            None => {
                // Channel closed before EndOfStream -> host-verified usage stays None.
                self.eof = true;
                Ok(None)
            }
        }
    }

    /// Returns host-verified terminal usage ONLY after the stream reached EOF
    /// with a verified `EndOfStream` accounting record.
    #[must_use]
    pub fn terminal_usage(&self) -> Option<VerifiedInferenceUsage> {
        if self.eof {
            self.completed_usage.clone()
        } else {
            None
        }
    }
}

impl Drop for InferenceStream {
    fn drop(&mut self) {
        self.cancel.cancel_sync();
    }
}
