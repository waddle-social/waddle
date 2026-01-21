//! Service Discovery (XEP-0030) implementation.
//!
//! Implements disco#info and disco#items for XMPP service discovery,
//! allowing clients to query server capabilities and available services.
//!
//! ## Supported Queries
//!
//! - **Server disco#info**: Returns server identity and supported features
//! - **Server disco#items**: Returns available services (MUC component)
//! - **MUC disco#info**: Returns MUC identity and features
//! - **MUC disco#items**: Returns list of available rooms
//!
//! ## Features Advertised
//!
//! - `http://jabber.org/protocol/disco#info`
//! - `http://jabber.org/protocol/disco#items`
//! - `urn:xmpp:mam:2` (Message Archive Management)
//! - `http://jabber.org/protocol/muc` (Multi-User Chat)
//! - `urn:xmpp:sm:3` (Stream Management)

pub mod info;
pub mod items;

pub use info::{
    build_disco_info_response, is_disco_info_query, parse_disco_info_query, DiscoInfoQuery,
    Feature, Identity, DISCO_INFO_NS,
};
pub use items::{
    build_disco_items_response, is_disco_items_query, parse_disco_items_query, DiscoItem,
    DiscoItemsQuery, DISCO_ITEMS_NS,
};
