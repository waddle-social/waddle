//! Closed-set call lifecycle reports sent once to the client's own server.

use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_xmpp_core::waddle_call_lifecycle::CallLifecycleReport;

use crate::bootstrap::NS_CLIENT;
use crate::client::ClientHandle;
use crate::error::ClientResult;

/// Build a call lifecycle IQ set without an explicit destination.
#[must_use]
pub fn build_call_lifecycle_report_iq(report: &CallLifecycleReport) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(xml_ncname!("type").to_owned(), "set")
        .attr(
            xml_ncname!("id").to_owned(),
            uuid::Uuid::new_v4().to_string(),
        )
        .append(report.build_element())
        .build()
}

/// Call lifecycle reporting on the async [`ClientHandle`].
pub trait CallLifecycleExt {
    /// Send this report once and return the raw IQ result.
    ///
    /// Callers must never retry a report, including after a failed request.
    ///
    /// # Errors
    /// Returns the IQ request's typed timeout, disconnection, stanza, or transport error.
    fn report_call_lifecycle<'a>(
        &'a self,
        report: &'a CallLifecycleReport,
    ) -> impl std::future::Future<Output = ClientResult<Element>> + Send + 'a;
}

impl CallLifecycleExt for ClientHandle {
    async fn report_call_lifecycle(&self, report: &CallLifecycleReport) -> ClientResult<Element> {
        self.send_iq(build_call_lifecycle_report_iq(report)).await
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::{broadcast, mpsc, watch};
    use waddle_xmpp_core::waddle_call_lifecycle::{
        CallClientPlatform, CallDurationBucket, CallEndReason, CallLifecycleKind,
        CallReconnectBucket, CallSetupOutcome, NS_WADDLE_CALL_LIFECYCLE,
    };

    use super::*;
    use crate::command::XmppCommand;
    use crate::error::{ClientError, StanzaError, StanzaErrorType};
    use crate::state::SessionSnapshot;

    fn report() -> CallLifecycleReport {
        CallLifecycleReport {
            kind: CallLifecycleKind::Muc,
            setup_outcome: CallSetupOutcome::Accepted,
            end_reason: CallEndReason::ReconnectExhausted,
            duration: CallDurationBucket::TenToSixtyMinutes,
            reconnects: CallReconnectBucket::Multiple,
            platform: CallClientPlatform::Android,
        }
    }

    #[test]
    fn call_lifecycle_iq_set_has_no_to_and_all_report_attributes() {
        let iq = build_call_lifecycle_report_iq(&report());
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.ns(), NS_CLIENT);
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), None);
        assert!(!iq.attr("id").expect("IQ id").is_empty());
        assert_eq!(iq.children().count(), 1);
        let payload = iq
            .get_child("report", NS_WADDLE_CALL_LIFECYCLE)
            .expect("report payload");
        assert_eq!(payload.attrs().len(), 6);
        assert_eq!(payload.attr("kind"), Some("muc"));
        assert_eq!(payload.attr("setup-outcome"), Some("accepted"));
        assert_eq!(payload.attr("end-reason"), Some("reconnect-exhausted"));
        assert_eq!(payload.attr("duration"), Some("10m-60m"));
        assert_eq!(payload.attr("reconnects"), Some("multiple"));
        assert_eq!(payload.attr("platform"), Some("android"));
        assert_eq!(payload.children().count(), 0);
    }

    #[tokio::test]
    async fn call_lifecycle_returns_reply_or_typed_error_without_retrying() {
        for result in [
            Ok(Element::builder("iq", NS_CLIENT)
                .attr(xml_ncname!("type").to_owned(), "result")
                .build()),
            Err(ClientError::StanzaError(StanzaError {
                error_type: StanzaErrorType::Modify,
                condition: "bad-request".to_owned(),
                text: None,
                application_condition: None,
            })),
        ] {
            let expect_success = result.is_ok();
            let (commands, mut receiver) = mpsc::channel(8);
            let (events, _) = broadcast::channel(8);
            let handle = ClientHandle::from_parts(
                commands,
                events,
                watch::Sender::new(SessionSnapshot::new()),
            );
            let driver = tokio::spawn(async move {
                let Some(XmppCommand::SendIq { stanza, responder }) = receiver.recv().await else {
                    panic!("expected one SendIq command");
                };
                assert_eq!(stanza.attr("to"), None);
                responder.send(result).expect("report awaits its result");
                receiver
            });

            let result = handle.report_call_lifecycle(&report()).await;
            assert_eq!(result.is_ok(), expect_success);
            match result {
                Ok(reply) => assert_eq!(reply.attr("type"), Some("result")),
                Err(ClientError::StanzaError(error)) => assert_eq!(error.condition, "bad-request"),
                Err(error) => panic!("unexpected report error: {error}"),
            }
            let mut receiver = driver.await.expect("driver completes");
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }
}
