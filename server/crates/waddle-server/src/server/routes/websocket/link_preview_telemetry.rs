#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkPreviewTelemetryEvent {
    ResolverReady,
    ResolverBlocked,
    ResolverFailed,
    ResolverUnsupported,
    /// Deferred-resolve admission was refused because the per-node
    /// concurrency cap is exhausted (#1470); the lookup answered `failed`
    /// immediately instead of queueing.
    ResolverSaturated,
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
            Self::ResolverSaturated => "saturated",
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
    use std::cell::Cell;
    use std::sync::{Mutex, MutexGuard};

    static EVENTS: Mutex<Vec<LinkPreviewTelemetryEvent>> = Mutex::new(Vec::new());
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        static RECORDING: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) struct RecordingGuard {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for RecordingGuard {
        fn drop(&mut self) {
            RECORDING.with(|recording| recording.set(false));
        }
    }

    pub(crate) fn lock() -> RecordingGuard {
        let guard = TEST_LOCK.lock().expect("link preview event test lock");
        RECORDING.with(|recording| recording.set(true));
        RecordingGuard { _guard: guard }
    }

    pub(crate) async fn async_lock() -> RecordingGuard {
        lock()
    }

    pub(crate) fn clear() {
        EVENTS.lock().expect("link preview event recorder").clear();
    }

    pub(super) fn push(event: LinkPreviewTelemetryEvent) {
        if !RECORDING.with(Cell::get) {
            return;
        }
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
            LinkPreviewTelemetryEvent::ResolverSaturated.as_str(),
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
                "saturated",
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

    #[test]
    fn recorded_events_ignore_threads_without_recorder_guard() {
        let _events_guard = recorded_events::lock();
        recorded_events::clear();

        std::thread::spawn(|| {
            record_link_preview_event(LinkPreviewTelemetryEvent::CleanupReference);
        })
        .join()
        .expect("worker thread should finish");

        assert_eq!(
            recorded_events::take(),
            Vec::<LinkPreviewTelemetryEvent>::new(),
            "a recorder must not capture events from unrelated concurrent tests"
        );
    }
}
