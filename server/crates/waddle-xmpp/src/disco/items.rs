//! Service Discovery: disco#items handling.

pub use waddle_xmpp_core::disco::items::{
    build_disco_items_response, is_disco_items_query, DiscoItem, DiscoItemsQuery, DISCO_ITEMS_NS,
};
use xmpp_parsers::iq::Iq;

use crate::XmppError;

/// Parse a disco#items query from an IQ stanza.
pub fn parse_disco_items_query(iq: &Iq) -> Result<DiscoItemsQuery, XmppError> {
    waddle_xmpp_core::disco::items::parse_disco_items_query(iq).map_err(Into::into)
}
