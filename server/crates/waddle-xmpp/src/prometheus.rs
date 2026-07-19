//! Scrape-liveness stub for the legacy Prometheus text endpoint.
//!
//! Contract phase of #1330: every metric family that used to be hand-
//! rendered here now lives on the OTel meter family (see
//! `crate::telemetry`), and every retired `waddle_*` name keeps
//! answering through the Mimir recording-rule aliases in
//! `infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml`
//! (the `waddle-aliases-*` groups).
//! `/metrics` stays up so the Alloy scrape keeps generating the `up`
//! series (the ScrapeAbsent alert's input); the body is a constant
//! liveness gauge and nothing else. Adding a family back here is
//! forbidden — new metrics go through the create-at-increment macros
//! (server CLAUDE.md, Telemetry).
//!
//! The one non-render survivor is [`metrics_test_lock`]: the process-
//! global test mutex the OTel metric-reader seam
//! (`telemetry::test_support`) serializes on.

#[cfg(any(test, feature = "test-utils"))]
use std::sync::OnceLock;

#[cfg(any(test, feature = "test-utils"))]
static METRICS_TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// Process-global mutex serializing every test that touches the metrics
/// pipeline (the OTel reader seam acquires it; legacy direct users keep
/// working). Never used in production code.
#[cfg(any(test, feature = "test-utils"))]
pub fn metrics_test_lock() -> &'static tokio::sync::Mutex<()> {
    METRICS_TEST_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Render the scrape-liveness stub served at `/metrics`.
pub fn render_metrics() -> String {
    concat!(
        "# HELP waddle_scrape_ok Constant 1 while the process is serving /metrics.\n",
        "# TYPE waddle_scrape_ok gauge\n",
        "waddle_scrape_ok 1\n",
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_metrics_is_the_liveness_stub_and_nothing_else() {
        let rendered = render_metrics();
        assert_eq!(
            rendered,
            "# HELP waddle_scrape_ok Constant 1 while the process is serving /metrics.\n\
             # TYPE waddle_scrape_ok gauge\n\
             waddle_scrape_ok 1\n",
        );
        // #1330 contract phase: no retired family may sneak back into the
        // text surface — the OTel meters + Mimir aliases own them now.
        for retired in [
            "waddle_connected_users",
            "waddle_messages_",
            "waddle_broadcast_",
            "waddle_delivery_",
            "waddle_sm_",
            "waddle_pending_",
            "waddle_push_",
            "waddle_dnd_",
            "waddle_resolver_",
            "waddle_user_actor_",
            "waddle_room_count",
        ] {
            assert!(
                !rendered.contains(retired),
                "retired family prefix {retired} must not be rendered",
            );
        }
    }
}
