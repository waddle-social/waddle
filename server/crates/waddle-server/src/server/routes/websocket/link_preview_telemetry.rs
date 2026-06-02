#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkPreviewTelemetryEvent {
    ResolverReady,
    ResolverBlocked,
    ResolverFailed,
    ResolverUnsupported,
    CacheHit,
    CacheMiss,
    TokenInvalid,
    TokenExpired,
    CleanupReference,
}

impl LinkPreviewTelemetryEvent {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ResolverReady => "ready",
            Self::ResolverBlocked => "blocked",
            Self::ResolverFailed => "failed",
            Self::ResolverUnsupported => "unsupported",
            Self::CacheHit => "cache_hit",
            Self::CacheMiss => "cache_miss",
            Self::TokenInvalid => "token_invalid",
            Self::TokenExpired => "token_expired",
            Self::CleanupReference => "cleanup_reference",
        }
    }
}

pub(crate) fn record_link_preview_event(event: LinkPreviewTelemetryEvent) {
    #[cfg(test)]
    recorded_events::push(event);
    tracing::info!(
        link_preview.event = event.as_str(),
        "link preview resolver event"
    );
}

#[cfg(test)]
pub(crate) mod recorded_events {
    use super::LinkPreviewTelemetryEvent;
    use std::sync::{Mutex, MutexGuard};

    static EVENTS: Mutex<Vec<LinkPreviewTelemetryEvent>> = Mutex::new(Vec::new());
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static ASYNC_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) fn lock() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().expect("link preview event test lock")
    }

    pub(crate) async fn async_lock() -> tokio::sync::MutexGuard<'static, ()> {
        ASYNC_TEST_LOCK.lock().await
    }

    pub(crate) fn clear() {
        EVENTS.lock().expect("link preview event recorder").clear();
    }

    pub(super) fn push(event: LinkPreviewTelemetryEvent) {
        EVENTS
            .lock()
            .expect("link preview event recorder")
            .push(event);
    }

    pub(crate) fn take() -> Vec<LinkPreviewTelemetryEvent> {
        std::mem::take(&mut *EVENTS.lock().expect("link preview event recorder"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn telemetry_event_labels_cover_issue_829_outcomes() {
        let labels = [
            LinkPreviewTelemetryEvent::ResolverReady.as_str(),
            LinkPreviewTelemetryEvent::ResolverBlocked.as_str(),
            LinkPreviewTelemetryEvent::ResolverFailed.as_str(),
            LinkPreviewTelemetryEvent::ResolverUnsupported.as_str(),
            LinkPreviewTelemetryEvent::CacheHit.as_str(),
            LinkPreviewTelemetryEvent::CacheMiss.as_str(),
            LinkPreviewTelemetryEvent::TokenInvalid.as_str(),
            LinkPreviewTelemetryEvent::TokenExpired.as_str(),
            LinkPreviewTelemetryEvent::CleanupReference.as_str(),
        ];

        assert_eq!(
            labels,
            [
                "ready",
                "blocked",
                "failed",
                "unsupported",
                "cache_hit",
                "cache_miss",
                "token_invalid",
                "token_expired",
                "cleanup_reference"
            ]
        );
    }

    #[test]
    fn telemetry_events_emit_stable_info_field() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(CaptureWriter(buf.clone()))
                .finish(),
        );

        record_link_preview_event(LinkPreviewTelemetryEvent::ResolverBlocked);

        let logs = String::from_utf8(buf.lock().expect("capture buffer lock").clone())
            .expect("captured logs are valid UTF-8");
        assert!(
            logs.contains("\"link_preview.event\":\"blocked\""),
            "link preview telemetry must expose a stable INFO field for metrics extraction. Captured logs:\n{logs}"
        );
        assert!(
            !logs.contains("message body"),
            "telemetry must not include message bodies. Captured logs:\n{logs}"
        );
    }
}
