use std::fmt;

use jid::NodePart;

use super::address::{parse_localpart, CanonicalBareJid};
use super::domain::CanonicalDomain;
use super::error::IdentityError;

/// The longest localpart the account profile accepts, in bytes.
pub const ACCOUNT_LOCALPART_MAX_BYTES: usize = 64;

/// The localpart of an account this deployment provisions.
///
/// One to [`ACCOUNT_LOCALPART_MAX_BYTES`] bytes of lowercase ASCII letters,
/// digits, `.`, `_`, `-` and `+`. On this set Nodeprep (the pinned `jid`
/// parser), the PRECIS UsernameCaseMapped profile with the RFC 7622 §3.3.1
/// exclusions, and XEP-0106 unescaping all leave the text unchanged, so the
/// account has one spelling under every preparation a peer may apply and
/// cannot collide with another account through folding.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountLocalpart(NodePart);

impl AccountLocalpart {
    /// Accept `input` exactly; nothing is lowercased or removed.
    pub fn parse(input: &str) -> Result<Self, IdentityError> {
        if input.is_empty()
            || input.len() > ACCOUNT_LOCALPART_MAX_BYTES
            || !input.bytes().all(is_account_byte)
        {
            return Err(IdentityError::OutsideAccountProfile);
        }
        parse_localpart(input).map(Self)
    }

    /// The localpart text.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The account's canonical bare JID at `domain`.
    pub fn account_jid(&self, domain: &CanonicalDomain) -> CanonicalBareJid {
        CanonicalBareJid::from_node(&self.0, domain)
    }
}

impl fmt::Display for AccountLocalpart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn is_account_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-' | b'+')
}
