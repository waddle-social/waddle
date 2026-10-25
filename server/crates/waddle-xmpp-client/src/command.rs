//! Commands exchanged between native client handles and their driver.

use minidom::Element;
use tokio::sync::oneshot;

use crate::error::ClientResult;

/// A typed command sent from [`crate::client::ClientHandle`] to the driver task.
#[derive(Debug)]
#[non_exhaustive]
pub enum XmppCommand {
    /// Send a raw stanza (fire-and-forget).
    SendStanza(Element),
    /// Send an IQ stanza and await a correlated result or error response.
    SendIq {
        /// Typed stanza carried by this event or request.
        stanza: Element,
        /// Peer expected to answer the pending IQ request.
        responder: oneshot::Sender<ClientResult<Element>>,
    },
    /// Disconnect the session cleanly.
    Disconnect,
}
