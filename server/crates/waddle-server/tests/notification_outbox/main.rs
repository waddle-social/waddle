//! Integration suite for the notification pipeline, grouped by concern:
//! `waddle_server::notification_outbox`, the `notification_activity`
//! store and writers, and the `notification_settings_projection` store.
//! One test binary keeps link time and disk footprint down.

mod activity;
mod activity_store;
mod activity_writers;
mod blocking;
mod candidates;
mod payload;
mod publish;
mod schema;
mod settings_projection;
mod support;
mod suppression;
mod worker;
