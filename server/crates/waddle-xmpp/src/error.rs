//! Error types for the XMPP server.

use crate::xep::xep0050::AdHocCommandCondition;
use thiserror::Error;
use waddle_xmpp_core::CoreError;

use crate::ownership::Entity;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stanza_error_conditions() {
        assert_eq!(StanzaErrorCondition::BadRequest.as_str(), "bad-request");
        assert_eq!(
            StanzaErrorCondition::NotAuthorized.as_str(),
            "not-authorized"
        );
        assert_eq!(
            StanzaErrorCondition::ItemNotFound.as_str(),
            "item-not-found"
        );
    }
}

mod implementation;
pub use implementation::StanzaErrorCondition;
pub use implementation::StanzaErrorType;
pub use implementation::XmppError;
