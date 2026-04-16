use super::{MediaBackend, MediaBackendKind, MediaConfig};

/// Embedded (in-process) SFU backend marker.
///
/// Session creation currently lives in the HTTP/XMPP routes rather than
/// on this backend; this type exists so `kind()` can be reported at
/// startup and so the `MediaBackend` trait can grow a real
/// `create_session` method in a single place later without touching
/// every call-site again.
#[derive(Debug)]
pub struct EmbeddedSfuBackend {
    _config: MediaConfig,
}

impl EmbeddedSfuBackend {
    pub fn new(config: MediaConfig) -> Self {
        Self { _config: config }
    }
}

impl MediaBackend for EmbeddedSfuBackend {
    fn kind(&self) -> MediaBackendKind {
        MediaBackendKind::EmbeddedSfu
    }
}
