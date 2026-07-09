//! #806 regression: the per-handler `disco#info answered` dispatch traces
//! must be emitted at `INFO`, not `DEBUG`.
//!
//! Production runs at `INFO`. When a connection wedges (the #757 incident),
//! these traces expose the bounded subhandler category that answered, without
//! exporting client IQ ids or target JIDs. At `DEBUG` they are filtered out in
//! production and never surface. This test pins both the level and privacy
//! boundary with an `INFO`-max subscriber.

use super::super::handlers::iq::handle_iq;
use super::super::ConnectionPhase;
use super::create_test_websocket_state;
use jid::FullJid;
use std::io;
use std::sync::{Arc, Mutex};

/// Minimal in-memory [`tracing_subscriber::fmt::MakeWriter`] that captures
/// formatted log output into a shared buffer for assertions.
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

// Current-thread runtime so the thread-local `set_default` subscriber stays
// bound for the whole test: on the default multi-thread runtime the future (and
// any work inside `handle_iq`) can hop worker threads, losing the binding and
// making the log-capture assertion flaky.
#[tokio::test(flavor = "current_thread")]
async fn disco_info_answered_trace_surfaces_at_info_level() {
    // Capture only INFO-and-above so a DEBUG trace is filtered out exactly as
    // it would be in production. `set_default` binds the subscriber to this
    // (current-thread) test's thread for the duration of the guard, so events
    // emitted across `.await` points are captured.
    let buf = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(CaptureWriter(buf.clone()))
            .finish(),
    );

    let state = create_test_websocket_state().await;
    let full: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let phase = ConnectionPhase::ready(full, false);
    let frame = r#"<iq xmlns="jabber:client" id="private-session-iq-806" type="get" to="alice@example.com"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#;

    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &phase,
    )
    .await;
    assert!(
        !responses.is_empty(),
        "server disco#info must produce a response"
    );

    let logs = String::from_utf8(buf.lock().expect("capture buffer lock").clone())
        .expect("captured logs are valid UTF-8");
    assert!(
        logs.contains("disco#info answered"),
        "#806: the per-handler disco#info trace must be emitted at INFO so it \
         surfaces in production (which filters at INFO). Captured INFO logs:\n{logs}"
    );
    assert!(
        logs.contains("\"handler\""),
        "#806: the disco#info trace must keep its stable `handler` field for \
         grep/OTLP. Captured INFO logs:\n{logs}"
    );
    assert!(
        !logs.contains("private-session-iq-806") && !logs.contains("alice@example.com"),
        "disco discovery traces must retain category fields without exporting IQ ids or JIDs. Captured INFO logs:\n{logs}"
    );
}
