//! Where stanzas from established inbound streams go.

use minidom::Element;

use super::peer_stream::VerifiedPeerStream;

/// What a [`StanzaSink`] does with a stanza.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkVerdict {
    /// The sink took the stanza; the stream continues. Taking a stanza
    /// says nothing about what the sink does with it: it may answer it,
    /// route it or drop it.
    Accepted,
    /// The stanza is refused and the stream closes: with
    /// `policy-violation`, or with `improper-addressing` for a stanza
    /// offered through [`StanzaSink::offer_malformed`].
    Refuse,
}

/// Receives each stanza an established inbound stream carries, after the
/// codec has checked that its `to` is the stream's local domain and its
/// `from` the stream's authenticated remote domain (RFC 6120 §4.9.3).
///
/// The sink is called on the connection's task and must not wait on
/// anything a remote peer controls.
pub trait StanzaSink: Send + Sync {
    /// A stanza whose `to` and `from` are in the identity profile's
    /// canonical form.
    fn offer(&self, stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict;

    /// A stanza whose `to` or `from` is a valid address written in a
    /// spelling the identity profile refuses; both addresses are the
    /// canonical form of their RFC 7622 preparation. It must not be
    /// processed: a sink may answer it with a `jid-malformed` stanza error
    /// and keep the stream. By default it is refused.
    fn offer_malformed(&self, _stream: &VerifiedPeerStream, _stanza: Element) -> SinkVerdict {
        SinkVerdict::Refuse
    }
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
