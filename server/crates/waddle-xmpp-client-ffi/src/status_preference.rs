//! ADR-010 cross-device manual status over the UniFFI boundary, receive
//! side: the stored pick is fetched from the account's own
//! `urn:waddle:status-preference:0` PEP node, and live picks arrive as
//! [`crate::WaddleClientEvent::StatusPreferenceChanged`]. The wire shapes
//! and the own-account guard live in
//! `waddle_xmpp_client::status_preference`; this module maps the typed
//! core values onto FFI records.

use jid::{BareJid, Jid};
use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::error::STANZA_CONDITION_ITEM_NOT_FOUND;
use waddle_xmpp_client::status_preference::{
    build_status_preference_fetch_iq, fetch_result_from_matches_account,
    parse_status_preference_fetch_result,
};
use waddle_xmpp_client::ClientError;
use waddle_xmpp_core::waddle_status_preference::{ManualStatus, StatusPreference};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::{account_bare_jid, send_iq_with_timeout};
use crate::{WaddleClient, WaddleError};

/// A status the user pinned by hand (ADR-010): an RFC 6121 §4.7.2.1
/// `<show/>` value, or plain available.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleManualStatus {
    Available,
    /// `<show>chat</show>`, free for chat.
    Chat,
    Away,
    Dnd,
}

/// The account-wide presence mode (ADR-010 decision 5). `Automatic` lets
/// each device's auto-away govern; `Manual` pins one status on every
/// device until the user resets it.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleStatusPreference {
    Automatic,
    Manual { status: WaddleManualStatus },
}

pub(crate) fn status_preference_to_ffi(preference: StatusPreference) -> WaddleStatusPreference {
    match preference {
        StatusPreference::Automatic => WaddleStatusPreference::Automatic,
        StatusPreference::Manual(status) => WaddleStatusPreference::Manual {
            status: match status {
                ManualStatus::Available => WaddleManualStatus::Available,
                ManualStatus::Chat => WaddleManualStatus::Chat,
                ManualStatus::Away => WaddleManualStatus::Away,
                ManualStatus::Dnd => WaddleManualStatus::Dnd,
            },
        },
    }
}

/// The preference a fetch result carries. A result from anyone but the
/// account's own PEP service (XEP-0223 §Security Considerations) and an
/// empty node both mean Automatic: absence never pins a status.
pub(crate) fn status_preference_from_fetch_result(
    result: &Element,
    account: &BareJid,
) -> WaddleStatusPreference {
    let from = result
        .attr("from")
        .and_then(|from| from.parse::<Jid>().ok());
    let spoofed = result.attr("from").is_some() && from.is_none();
    if spoofed || !fetch_result_from_matches_account(from.as_ref(), account) {
        return WaddleStatusPreference::Automatic;
    }
    parse_status_preference_fetch_result(result)
        .map(status_preference_to_ffi)
        .unwrap_or(WaddleStatusPreference::Automatic)
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// The account's stored presence mode from its own
    /// `urn:waddle:status-preference:0` PEP node; `Automatic` when none is
    /// stored. Errors (not connected, timeout, other stanza errors) let
    /// the caller keep what it knew.
    pub async fn fetch_status_preference(&self) -> Result<WaddleStatusPreference, WaddleError> {
        let account = account_bare_jid(&self.config.jid).map_err(|_| WaddleError::InvalidJid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = build_status_preference_fetch_iq(&Uuid::new_v4().to_string());
        match send_iq_with_timeout(&handle, iq).await {
            Ok(result) => Ok(status_preference_from_fetch_result(&result, &account)),
            Err(ClientError::StanzaError(err))
                if err.condition == STANZA_CONDITION_ITEM_NOT_FOUND =>
            {
                Ok(WaddleStatusPreference::Automatic)
            }
            Err(e) => Err(client_error_to_waddle(&e)),
        }
    }
}
