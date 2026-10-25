//! Database-backed storage for XEP-0060 PubSub and XEP-0163 PEP data.

use crate::db::Row;
use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp::pubsub::{
    Affiliation, NodeConfig, PubSubItem, PubSubNode, PubSubStorage, StoredItem, SubId,
    Subscription, SubscriptionState,
};
use waddle_xmpp::XmppError;

use crate::db::Database;

mod database;
mod item;
mod node;
mod open;
mod schema;
mod versioned;
#[cfg(test)]
mod versioned_tests;

pub use open::{build_database_pubsub_storage, build_pubsub_storage};

#[cfg(test)]
mod tests;

mod implementation;
pub use implementation::DatabasePubSubStorage;
