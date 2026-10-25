mod acme;
pub(crate) mod caps_resolution;
mod config;
pub(crate) mod dual_registration;
pub(crate) mod durable_membership;
pub(crate) mod extension_bot;
pub(crate) mod extension_bot_avatar;
pub(crate) mod extension_bot_rooms;
mod extension_commands;
pub mod extension_host_adapter;
mod extension_host_tools;
mod fixed_account;
mod health;
mod http;
pub(crate) mod process_metrics;
pub(crate) mod profile_publish_route;
mod room_registry_gauge;
pub(crate) mod session_janitors;
mod state;
pub(crate) mod state_inventory;
pub(crate) mod state_inventory_metrics;
pub(crate) mod state_inventory_route;
mod topology;
mod trace;
#[cfg(test)]
mod xmpp_account_state;
#[cfg(test)]
mod xmpp_app_state;
#[cfg(test)]
mod xmpp_auth_state;
mod xmpp_channels;
#[cfg(test)]
mod xmpp_permission_state;
#[cfg(test)]
mod xmpp_profile_state;
#[cfg(test)]
mod xmpp_roster_state;
#[cfg(test)]
mod xmpp_space_state;
#[cfg(test)]
mod xmpp_upload_state;
#[cfg(test)]
mod xmpp_user_storage_state;

pub(crate) mod bootstrap_membership;
pub(crate) mod managed_channel_policy;
pub(crate) mod routes;

pub use routes::interpret::effects;
/// Interpreter-only ingress planning seam, consumed by the authority cutover.
pub use routes::interpret::{build_plan_deps, plan_message_dispatch};
pub mod xmpp_state;

pub use config::{XmppAcmeConfig, XmppConfig};
pub use state::{resolve_server_owner_jids, AppState, AppStateDeps};

use crate::config::ServerConfig;
use crate::db::DatabasePool;
use crate::permissions::PermissionActor;
use crate::spaces_metadata::DatabaseSpacesMetadataStore;
use acme::start_acme_runtime;
use anyhow::Result;
use fixed_account::{ensure_fixed_test_account, fixed_test_account_enabled};
use http::{create_websocket_mam_storage, start_http_server, HttpServerDeps};
use kameo::actor::Spawn;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

#[cfg(test)]
mod tests;

mod lifecycle;
use lifecycle::bootstrap_and_register_store_lineage;
use lifecycle::bootstrap_mam_lineage;
pub(crate) use lifecycle::bootstrap_store_lineage;
pub use lifecycle::start;
pub use lifecycle::start_with_config;
