use jid::BareJid;
use waddle_xmpp::telemetry::attributes::{CallSignalEvent, MetricAttribute, SfuDenialReason};

pub(crate) enum CallSignalTarget<'a> {
    Peer(&'a BareJid),
    Room(&'a BareJid),
}

pub(crate) fn record_call_signal(
    event: CallSignalEvent,
    user: &BareJid,
    target: Option<CallSignalTarget<'_>>,
) {
    match target {
        Some(CallSignalTarget::Peer(peer)) => tracing::info!(
            event = event.value(),
            user = %user,
            peer = %peer,
            "call signaling event"
        ),
        Some(CallSignalTarget::Room(room)) => tracing::info!(
            event = event.value(),
            user = %user,
            room = %room,
            "call signaling event"
        ),
        None => tracing::info!(
            event = event.value(),
            user = %user,
            "call signaling event"
        ),
    }
    waddle_xmpp::counter_add!(
        "waddle.call.signaling",
        "1",
        "JMI and Muji call-signaling events.",
        1,
        event,
    );
}

pub(crate) fn record_sfu_token_denial(room: &BareJid, user: &BareJid, reason: SfuDenialReason) {
    tracing::warn!(
        room = %room,
        user = %user,
        reason = reason.value(),
        "SFU token request denied"
    );
    waddle_xmpp::telemetry::call::increment_sfu_token_denied(reason);
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

    #[tokio::test(flavor = "current_thread")]
    async fn jmi_signal_emits_info_with_bare_jids_and_counter() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let user: BareJid = "alice@example.com".parse().expect("valid user JID");
        let peer: BareJid = "bob@example.com".parse().expect("valid peer JID");

        record_call_signal(
            CallSignalEvent::JmiPropose,
            &user,
            Some(CallSignalTarget::Peer(&peer)),
        );

        assert_eq!(
            metrics.counter_sum("waddle.call.signaling", &[("event", "jmi_propose")]),
            Some(1)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.signaling"),
            Some("1".to_string())
        );
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(logs.contains("\"level\":\"INFO\""), "{logs}");
        assert!(logs.contains("jmi_propose"), "{logs}");
        assert!(logs.contains("alice@example.com"), "{logs}");
        assert!(logs.contains("bob@example.com"), "{logs}");
    }
}
