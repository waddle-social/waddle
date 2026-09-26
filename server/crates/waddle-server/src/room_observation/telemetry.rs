use std::time::Duration;

use waddle_extensions::{PluginId, RoomObservationOutcome, Timestamp};

pub(super) fn started(observed_at: &Timestamp, attempt: u32) {
    if attempt == 1 {
        waddle_xmpp::histogram_record!(
            "waddle.extension.observation.queue.delay",
            "ms",
            "Time from committed room source to its first extension invocation.",
            source_age_ms(observed_at),
        );
    }
}

pub(super) fn finished(
    plugin: &PluginId,
    attempt: u32,
    outcome: &RoomObservationOutcome,
    duration: Duration,
    saved: bool,
) {
    let category = match outcome {
        RoomObservationOutcome::Completed(_) => "completed",
        RoomObservationOutcome::RetryableFailure(_) => "retryable_failure",
        RoomObservationOutcome::PermanentFailure(_) => "permanent_failure",
        RoomObservationOutcome::NotApplicable(_) => "skipped",
    };
    waddle_xmpp::histogram_record!(
        "waddle.extension.observation.invocation.duration",
        "ms",
        "Wall time of a room extension invocation including its provider request.",
        duration.as_secs_f64() * 1_000.0,
    );
    // Fixed categories and numeric timing only: never log input text, provider
    // responses, guest error strings, or secret configuration.
    tracing::info!(plugin = %plugin, attempt, category, saved,
        duration_ms = duration.as_millis(), "room extension observation finished");
}

pub(super) fn published(observed_at: &Timestamp) {
    waddle_xmpp::counter_add!(
        "waddle.extension.observation.published",
        "1",
        "Saved room extension results admitted to the durable room broadcast path.",
        1,
    );
    waddle_xmpp::histogram_record!(
        "waddle.extension.observation.publication.delay",
        "ms",
        "Time from committed room source to durable room result publication.",
        source_age_ms(observed_at),
    );
}

fn source_age_ms(observed_at: &Timestamp) -> f64 {
    chrono::DateTime::parse_from_rfc3339(observed_at.as_str()).map_or(0.0, |time| {
        crate::time::now_ms()
            .saturating_sub(time.timestamp_millis())
            .max(0) as f64
    })
}
