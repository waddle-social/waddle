//! One-shot call lifecycle reports to the client's own server.

use minidom::Element;
use waddle_xmpp_client::call_lifecycle::build_call_lifecycle_report_iq;
use waddle_xmpp_client::ClientError;
use waddle_xmpp_core::waddle_call_lifecycle::{
    CallClientPlatform, CallDurationBucket, CallEndReason, CallLifecycleKind, CallLifecycleReport,
    CallReconnectBucket, CallSetupOutcome,
};

use crate::messaging_verbs::send_iq_with_timeout;
use crate::WaddleClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleCallLifecycleKind {
    Dm,
    Muc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleCallSetupOutcome {
    Proposed,
    Accepted,
    Declined,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleCallEndReason {
    Hangup,
    PeerLeft,
    Error,
    ReconnectExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleCallDurationBucket {
    None,
    UnderOneMinute,
    OneToTenMinutes,
    TenToSixtyMinutes,
    OverSixtyMinutes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleCallReconnectBucket {
    None,
    Once,
    Multiple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct WaddleCallLifecycleReport {
    pub kind: WaddleCallLifecycleKind,
    pub setup_outcome: WaddleCallSetupOutcome,
    pub end_reason: WaddleCallEndReason,
    pub duration: WaddleCallDurationBucket,
    pub reconnects: WaddleCallReconnectBucket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleCallLifecycleReportOutcome {
    Accepted,
    Unsupported,
    RateLimited,
    Failed,
}

/// Map the native arguments to the shared IQ builder without choosing a platform.
pub(crate) fn call_lifecycle_report_iq(
    report: WaddleCallLifecycleReport,
    platform: CallClientPlatform,
) -> Element {
    build_call_lifecycle_report_iq(&CallLifecycleReport {
        kind: match report.kind {
            WaddleCallLifecycleKind::Dm => CallLifecycleKind::Dm,
            WaddleCallLifecycleKind::Muc => CallLifecycleKind::Muc,
        },
        setup_outcome: match report.setup_outcome {
            WaddleCallSetupOutcome::Proposed => CallSetupOutcome::Proposed,
            WaddleCallSetupOutcome::Accepted => CallSetupOutcome::Accepted,
            WaddleCallSetupOutcome::Declined => CallSetupOutcome::Declined,
            WaddleCallSetupOutcome::Timeout => CallSetupOutcome::Timeout,
            WaddleCallSetupOutcome::Failed => CallSetupOutcome::Failed,
        },
        end_reason: match report.end_reason {
            WaddleCallEndReason::Hangup => CallEndReason::Hangup,
            WaddleCallEndReason::PeerLeft => CallEndReason::PeerLeft,
            WaddleCallEndReason::Error => CallEndReason::Error,
            WaddleCallEndReason::ReconnectExhausted => CallEndReason::ReconnectExhausted,
        },
        duration: match report.duration {
            WaddleCallDurationBucket::None => CallDurationBucket::None,
            WaddleCallDurationBucket::UnderOneMinute => CallDurationBucket::Under1m,
            WaddleCallDurationBucket::OneToTenMinutes => CallDurationBucket::OneToTenMinutes,
            WaddleCallDurationBucket::TenToSixtyMinutes => CallDurationBucket::TenToSixtyMinutes,
            WaddleCallDurationBucket::OverSixtyMinutes => CallDurationBucket::Over60m,
        },
        reconnects: match report.reconnects {
            WaddleCallReconnectBucket::None => CallReconnectBucket::None,
            WaddleCallReconnectBucket::Once => CallReconnectBucket::Once,
            WaddleCallReconnectBucket::Multiple => CallReconnectBucket::Multiple,
        },
        platform,
    })
}

/// Unsupported host targets use the existing `web` bucket; only macOS uses `macos`.
fn call_lifecycle_platform() -> CallClientPlatform {
    #[cfg(target_os = "ios")]
    {
        CallClientPlatform::Ios
    }
    #[cfg(target_os = "macos")]
    {
        CallClientPlatform::Macos
    }
    #[cfg(target_os = "android")]
    {
        CallClientPlatform::Android
    }
    #[cfg(not(any(target_os = "ios", target_os = "macos", target_os = "android")))]
    {
        CallClientPlatform::Web
    }
}

pub(crate) fn classify_call_lifecycle_result(
    result: &Result<Element, ClientError>,
) -> WaddleCallLifecycleReportOutcome {
    match result {
        Ok(reply) if reply.attr("type") == Some("result") => {
            WaddleCallLifecycleReportOutcome::Accepted
        }
        Err(ClientError::StanzaError(error)) => match error.condition.as_str() {
            "feature-not-implemented" | "service-unavailable" => {
                WaddleCallLifecycleReportOutcome::Unsupported
            }
            "policy-violation" => WaddleCallLifecycleReportOutcome::RateLimited,
            _ => WaddleCallLifecycleReportOutcome::Failed,
        },
        _ => WaddleCallLifecycleReportOutcome::Failed,
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Submit one report, with a bounded wait and no retry on any outcome.
    /// The caller must invoke this at most once for each report, even after failure.
    pub async fn report_call_lifecycle(
        &self,
        report: WaddleCallLifecycleReport,
    ) -> WaddleCallLifecycleReportOutcome {
        let Some(handle) = self.clone_handle().await else {
            return WaddleCallLifecycleReportOutcome::Failed;
        };
        let iq = call_lifecycle_report_iq(report, call_lifecycle_platform());
        classify_call_lifecycle_result(&send_iq_with_timeout(&handle, iq).await)
    }
}
