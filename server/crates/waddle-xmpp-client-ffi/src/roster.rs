//! The account's contact list (RFC 6121 §2 roster).
//!
//! A native client lists the people an account knows from here: presence
//! alone cannot, since a contact who is offline sends none.

use minidom::Element;

use waddle_xmpp_client::{build_roster_get_iq, parse_roster_result, Subscription};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};

/// One contact on the account's roster: its bare JID and the name the
/// account saved for it, when it saved one.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WaddleRosterContact {
    pub jid: String,
    pub name: Option<String>,
}

/// The contacts in an RFC 6121 §2.1.4 roster result. A result the roster
/// cannot be read from (no `<query/>`, or an item without a valid JID) is
/// an error, never an empty list: a caller that takes "no contacts" as
/// fact would show an account with contacts as having none.
pub(crate) fn roster_contacts_from_result(
    result: &Element,
) -> Result<Vec<WaddleRosterContact>, WaddleError> {
    let roster = parse_roster_result(result).ok_or(WaddleError::MalformedResponse)?;
    Ok(roster
        .items
        .into_iter()
        // `remove` only appears in a roster push; it names a contact
        // that is gone.
        .filter(|item| item.subscription != Subscription::Remove)
        .map(|item| WaddleRosterContact {
            jid: item.jid.to_string(),
            name: item.name.filter(|name| !name.trim().is_empty()),
        })
        .collect())
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// RFC 6121 §2.1.3: fetch the account's roster (wasm
    /// `list_roster_contacts` parity). An account with no contacts
    /// answers an empty list; a lookup that fails or cannot be read is
    /// an error.
    pub async fn fetch_roster_contacts(&self) -> Result<Vec<WaddleRosterContact>, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let result = send_iq_with_timeout(&handle, build_roster_get_iq(None, None))
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        roster_contacts_from_result(&result)
    }
}
