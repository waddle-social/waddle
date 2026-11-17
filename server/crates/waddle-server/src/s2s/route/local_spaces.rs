//! The Spaces of this deployment as accounts at other servers use them:
//! what the router hands to the Spaces host and what it expects back
//! (ADR-0020 D10; XEP-0503 over XEP-0060).
//!
//! The router decides transport questions: which stream carried a request,
//! the bounds on what one remote domain and one of its accounts may ask,
//! and what leaves for a remote JID. Everything about a Space is the
//! host's: whether it is open to other servers, and whether a principal
//! may discover it, read it, subscribe to it or change it, decided for that
//! principal on the evidence of its stream at each request
//! ([`SpaceRequest::principal`]). Reaching the host admits nothing.
//!
//! A Space is a PubSub node in the database every replica shares, so the
//! replica a stream lands on decides the request itself: nothing is carried
//! to another replica.

use async_trait::async_trait;
use jid::BareJid;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

use super::principal::RemotePrincipal;
use super::reach::SpaceOperation;

/// One IQ request a remote principal addressed to the Spaces PubSub
/// service.
#[derive(Debug, Clone)]
pub struct SpaceRequest {
    /// The sender, as the inbound stream that carried the request proved
    /// it. Every decision the host takes for this request names the
    /// evidence of that stream.
    pub principal: RemotePrincipal,
    /// What the request is, as [`super::reach`] classified it.
    pub operation: SpaceOperation,
    /// The request: an IQ `get` or `set`.
    pub stanza: Stanza,
}

/// A publication of a local Space whose event notification may be owed to
/// subscribers at other servers, handed over once the publication is
/// stored.
#[derive(Debug, Clone)]
pub struct SpacePublication {
    /// The Spaces PubSub service.
    pub service: BareJid,
    /// The Space's node.
    pub node: String,
    /// The creation time, in milliseconds, of the node the publication was
    /// made to: a Space deleted and created again under the name since is
    /// another Space, and is told nothing of it.
    pub node_created_at_ms: i64,
    /// The event notification as the service built it for its own
    /// subscribers (XEP-0060 §7.1).
    pub event: Message,
}

/// The Spaces of this deployment, as the S2S router reaches them.
///
/// [`LocalSpaces::handle`] is called off the stream's task, one request of
/// a remote account at a time, in the order they arrived.
#[async_trait]
pub trait LocalSpaces: Send + Sync {
    /// Decide one request and answer it: the IQ result or error for its
    /// sender, from the address it was sent to. An empty answer drops the
    /// request.
    async fn handle(&self, request: SpaceRequest) -> Vec<Stanza>;

    /// Take `publication` for its subscribers at other servers. It only
    /// queues: who is notified is decided later, off the publisher's
    /// request. `false` when it was not taken, and nothing is kept for it.
    fn published(&self, _publication: SpacePublication) -> bool {
        false
    }

    /// Wait until every publication taken before this call has been
    /// decided and its notifications handed to the router.
    async fn publications_settled(&self) {}
}

/// The most stanzas the router sends back for one request to the Spaces
/// service. A request has one answer; anything beyond is dropped.
pub const MAX_SPACE_REPLIES: usize = 1;
