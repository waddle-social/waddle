//! Wire factories and outcome handling for call lifecycle reports.

use std::time::Duration;

use minidom::Element;
use waddle_xmpp_client::{ClientError, StanzaError, StanzaErrorType};
use waddle_xmpp_core::waddle_call_lifecycle::{CallClientPlatform, NS_WADDLE_CALL_LIFECYCLE};
use xmpp_parsers::iq::Iq;

use crate::call_lifecycle::{call_lifecycle_report_iq, classify_call_lifecycle_result};
use crate::{
    WaddleCallDurationBucket, WaddleCallEndReason, WaddleCallLifecycleKind,
    WaddleCallLifecycleReport, WaddleCallLifecycleReportOutcome, WaddleCallReconnectBucket,
    WaddleCallSetupOutcome, WaddleClient, WaddleClientEvent, WaddleConfig, WaddleEventListener,
};

fn sample_report() -> WaddleCallLifecycleReport {
    WaddleCallLifecycleReport {
        kind: WaddleCallLifecycleKind::Dm,
        setup_outcome: WaddleCallSetupOutcome::Accepted,
        end_reason: WaddleCallEndReason::Hangup,
        duration: WaddleCallDurationBucket::OneToTenMinutes,
        reconnects: WaddleCallReconnectBucket::Once,
    }
}

#[test]
fn call_lifecycle_factory_covers_every_platform() {
    for (platform, token) in [
        (CallClientPlatform::Ios, "ios"),
        (CallClientPlatform::Macos, "macos"),
        (CallClientPlatform::Android, "android"),
        (CallClientPlatform::Web, "web"),
    ] {
        let iq = call_lifecycle_report_iq(sample_report(), platform);
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.ns(), "jabber:client");
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), None);
        assert!(!iq.attr("id").expect("IQ id").is_empty());
        assert_eq!(iq.children().count(), 1);
        let payload = iq
            .get_child("report", NS_WADDLE_CALL_LIFECYCLE)
            .expect("report payload");
        assert_eq!(payload.attrs().len(), 6);
        assert_eq!(payload.children().count(), 0);
        for (name, value) in [
            ("kind", "dm"),
            ("setup-outcome", "accepted"),
            ("end-reason", "hangup"),
            ("duration", "1m-10m"),
            ("reconnects", "once"),
            ("platform", token),
        ] {
            assert_eq!(payload.attr(name), Some(value));
        }
    }
}

#[test]
fn call_lifecycle_factory_maps_every_native_enum_variant() {
    for (kind, token) in [
        (WaddleCallLifecycleKind::Dm, "dm"),
        (WaddleCallLifecycleKind::Muc, "muc"),
    ] {
        assert_attribute(
            WaddleCallLifecycleReport {
                kind,
                ..sample_report()
            },
            "kind",
            token,
        );
    }
    for (setup_outcome, token) in [
        (WaddleCallSetupOutcome::Proposed, "proposed"),
        (WaddleCallSetupOutcome::Accepted, "accepted"),
        (WaddleCallSetupOutcome::Declined, "declined"),
        (WaddleCallSetupOutcome::Timeout, "timeout"),
        (WaddleCallSetupOutcome::Failed, "failed"),
    ] {
        assert_attribute(
            WaddleCallLifecycleReport {
                setup_outcome,
                ..sample_report()
            },
            "setup-outcome",
            token,
        );
    }
    for (end_reason, token) in [
        (WaddleCallEndReason::Hangup, "hangup"),
        (WaddleCallEndReason::PeerLeft, "peer-left"),
        (WaddleCallEndReason::Error, "error"),
        (
            WaddleCallEndReason::ReconnectExhausted,
            "reconnect-exhausted",
        ),
    ] {
        assert_attribute(
            WaddleCallLifecycleReport {
                end_reason,
                ..sample_report()
            },
            "end-reason",
            token,
        );
    }
    for (duration, token) in [
        (WaddleCallDurationBucket::None, "none"),
        (WaddleCallDurationBucket::UnderOneMinute, "under-1m"),
        (WaddleCallDurationBucket::OneToTenMinutes, "1m-10m"),
        (WaddleCallDurationBucket::TenToSixtyMinutes, "10m-60m"),
        (WaddleCallDurationBucket::OverSixtyMinutes, "over-60m"),
    ] {
        assert_attribute(
            WaddleCallLifecycleReport {
                duration,
                ..sample_report()
            },
            "duration",
            token,
        );
    }
    for (reconnects, token) in [
        (WaddleCallReconnectBucket::None, "none"),
        (WaddleCallReconnectBucket::Once, "once"),
        (WaddleCallReconnectBucket::Multiple, "multiple"),
    ] {
        assert_attribute(
            WaddleCallLifecycleReport {
                reconnects,
                ..sample_report()
            },
            "reconnects",
            token,
        );
    }
}

fn assert_attribute(report: WaddleCallLifecycleReport, name: &str, token: &str) {
    let iq = call_lifecycle_report_iq(report, CallClientPlatform::Ios);
    assert_eq!(
        iq.get_child("report", NS_WADDLE_CALL_LIFECYCLE)
            .expect("report payload")
            .attr(name),
        Some(token),
    );
}

#[test]
fn call_lifecycle_result_is_accepted() {
    let reply = Iq::Result {
        from: None,
        to: None,
        id: "report-test".to_owned(),
        payload: None,
    }
    .into();
    assert_eq!(
        classify_call_lifecycle_result(&Ok(reply)),
        WaddleCallLifecycleReportOutcome::Accepted,
    );
}

#[test]
fn call_lifecycle_stanza_errors_map_to_outcomes() {
    for (condition, expected) in [
        (
            "feature-not-implemented",
            WaddleCallLifecycleReportOutcome::Unsupported,
        ),
        (
            "service-unavailable",
            WaddleCallLifecycleReportOutcome::Unsupported,
        ),
        (
            "policy-violation",
            WaddleCallLifecycleReportOutcome::RateLimited,
        ),
        ("bad-request", WaddleCallLifecycleReportOutcome::Failed),
    ] {
        let error = ClientError::StanzaError(StanzaError {
            error_type: StanzaErrorType::Cancel,
            condition: condition.to_owned(),
            text: None,
            application_condition: None,
        });
        assert_eq!(classify_call_lifecycle_result(&Err(error)), expected);
    }
}

#[test]
fn call_lifecycle_timeout_and_disconnect_fail() {
    for error in [
        ClientError::IqTimeout {
            timeout: Duration::from_secs(1),
        },
        ClientError::Disconnected,
        ClientError::TransportClosed,
    ] {
        assert_eq!(
            classify_call_lifecycle_result(&Err(error)),
            WaddleCallLifecycleReportOutcome::Failed,
        );
    }
}

#[test]
fn call_lifecycle_unexpected_reply_fails() {
    let reply = Element::builder("iq", "jabber:client").build();
    assert_eq!(
        classify_call_lifecycle_result(&Ok(reply)),
        WaddleCallLifecycleReportOutcome::Failed,
    );
}

struct NoopListener;

impl WaddleEventListener for NoopListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

#[tokio::test]
async fn call_lifecycle_not_connected_fails() {
    let client = WaddleClient::new(
        WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_owned(),
            jid: "alice@waddle.test".to_owned(),
            access_token: "token".to_owned(),
            resource: "test".to_owned(),
            resumable: true,
        },
        Box::new(NoopListener),
    );
    assert_eq!(
        client.report_call_lifecycle(sample_report()).await,
        WaddleCallLifecycleReportOutcome::Failed,
    );
}
