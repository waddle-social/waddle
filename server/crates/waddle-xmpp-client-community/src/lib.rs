//! Typed community pubsub operations used by the native and browser clients.

pub mod calendar;
pub mod calendar_edit;
pub mod extension_route_types;
pub mod extension_routes;
pub mod feed;
pub mod pep_stories;
pub mod plan_workspace;
pub mod planning;
pub mod stories;
pub mod story_reads;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InvalidInput(String);

fn invalid_input(reason: impl Into<String>) -> InvalidInput {
    InvalidInput(reason.into())
}

pub mod remote_address;
pub mod remote_bookmark;
pub mod remote_community;
pub mod remote_preferences;
