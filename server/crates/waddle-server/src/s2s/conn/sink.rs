//! Where stanzas from established inbound streams go.

use minidom::Element;

use super::peer_stream::VerifiedPeerStream;

/// What a [`StanzaSink`] does with a stanza.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkVerdict {
    /// The stanza is refused; the stream closes with `policy-violation`.
    Refuse,
}

/// Receives each stanza an established inbound stream carries, after the
/// codec has checked that its `to` is the stream's local domain and its
/// `from` the stream's authenticated remote domain (RFC 6120 §4.9.3).
///
/// The sink is called on the connection's task and must not wait on
/// anything a remote peer controls.
pub trait StanzaSink: Send + Sync {
    fn offer(&self, stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict;
}

/// The sink of this release: no stanza is routed, stored or answered, so
/// every stanza is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct RefuseAllStanzas;

impl StanzaSink for RefuseAllStanzas {
    fn offer(&self, _stream: &VerifiedPeerStream, _stanza: Element) -> SinkVerdict {
        SinkVerdict::Refuse
    }
}
