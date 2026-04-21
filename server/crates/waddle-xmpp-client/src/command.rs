use minidom::Element;
use tokio::sync::oneshot;

use crate::error::ClientResult;

/// A typed command sent from [`crate::client::ClientHandle`] to the driver task.
#[derive(Debug)]
pub enum XmppCommand {
    /// Send a raw stanza (fire-and-forget).
    SendStanza(Element),
    /// Send an IQ stanza and await a correlated result or error response.
    SendIq {
        stanza: Element,
        responder: oneshot::Sender<ClientResult<Element>>,
    },
    /// Disconnect the session cleanly.
    Disconnect,
}
