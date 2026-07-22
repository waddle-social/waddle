//! XEP-0430 inbox sync over the UniFFI boundary.
//!
//! `fetch_inbox` drives the native [`InboxExt`] streamed-query
//! reducer (subscribe-before-send, entries folded by MAM `queryid`,
//! resolved on the closing `<fin/>`); `mark_inbox_read` wraps the
//! Waddle `urn:waddle:inbox:0` `<mark-read/>` IQ. Both verbs mirror
//! the wasm `fetch_inbox` / `mark_inbox_read` surface, including its
//! option polarity (`only_unread`, `no_messages`).

use jid::BareJid;

use waddle_xmpp_client::inbox::InboxPage;
use waddle_xmpp_client::InboxExt;

use crate::convert::inbox_entry_to_ffi;
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::IQ_TIMEOUT;
use crate::{WaddleClient, WaddleError, WaddleInboxResult};

/// Folded streamed page → FFI result. The `<fin/>` counts carry the
/// server-wide unread sum (`all-unread`) alongside the page-scoped
/// totals; entry conversion is shared with the `InboxPush` event path.
pub(crate) fn inbox_page_to_ffi(page: InboxPage) -> WaddleInboxResult {
    WaddleInboxResult {
        total: page.fin.counts.total,
        unread: page.fin.counts.unread,
        all_unread: page.fin.counts.all_unread,
        conversations: page.entries.into_iter().map(inbox_entry_to_ffi).collect(),
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Run one XEP-0430 `<inbox xmlns='urn:xmpp:inbox:1'/>` query and
    /// resolve once the server's closing `<fin/>` arrives.
    ///
    /// `only_unread` maps to `unread-only='true'`; `no_messages`
    /// inverts the XEP's `messages` attribute (pass `true` to elide
    /// the embedded MAM `<result/>` bodies — the cheap hydration
    /// shape the clients use on session-ready).
    pub async fn fetch_inbox(
        &self,
        only_unread: bool,
        no_messages: bool,
    ) -> Result<WaddleInboxResult, WaddleError> {
        // Single in-flight query only: `messages='false'` responses
        // carry no MAM queryid, so an overlapping query would fold
        // the union of both streams into each page (the wasm driver
        // defers overlapping queries for the same reason).
        let _gate = self.inbox_query_gate.lock().await;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        // The ext's send_iq has no request-level timeout; bound the
        // whole streamed round-trip like every other fetch verb.
        match tokio::time::timeout(IQ_TIMEOUT, handle.fetch_inbox(only_unread, !no_messages)).await
        {
            Ok(Ok(page)) => Ok(inbox_page_to_ffi(page)),
            Ok(Err(e)) => Err(client_error_to_waddle(&e)),
            Err(_) => Err(WaddleError::Timeout),
        }
    }

    /// Mark one conversation (optionally one thread of it) read via
    /// the Waddle `urn:waddle:inbox:0` `<mark-read/>` IQ. The server
    /// answers with a fresh unread state push, so callers only need
    /// to clear their local overlay optimistically.
    pub async fn mark_inbox_read(
        &self,
        partner_jid: String,
        thread_id: Option<String>,
    ) -> Result<(), WaddleError> {
        let partner: BareJid = partner_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let partner = partner.to_string();
        match tokio::time::timeout(
            IQ_TIMEOUT,
            handle.mark_inbox_read(&partner, thread_id.as_deref()),
        )
        .await
        {
            Ok(result) => result.map_err(|e| client_error_to_waddle(&e)),
            Err(_) => Err(WaddleError::Timeout),
        }
    }
}
