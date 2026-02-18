use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("DNS resolution failed: {0}")]
    DnsResolutionFailed(String),

    #[error("TLS handshake failed: {0}")]
    TlsHandshakeFailed(String),

    #[error("authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("stream error: {0}")]
    StreamError(String),

    #[error("connection timeout")]
    Timeout,

    #[error("transport error: {0}")]
    TransportError(String),
}

impl ConnectionError {
    pub fn is_retryable(&self) -> bool {
        !matches!(self, ConnectionError::AuthenticationFailed(_))
    }
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("stanza parse failed: {0}")]
    ParseFailed(String),

    #[error("processor failed: {0}")]
    ProcessorFailed(String),

    #[error("plugin hook timed out: {0}")]
    PluginTimeout(String),
}
