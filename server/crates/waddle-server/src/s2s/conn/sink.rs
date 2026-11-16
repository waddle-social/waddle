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

/// How an established stream ended, as a sink learns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// The stream ended in an orderly way: either side sent its closing
    /// stream tag, it was idle, a newer stream for the same pair replaced
    /// it, it was evicted or aged out, or this server is shutting down. A
    /// new stream carries the pair's next stanza and nothing is known to be
    /// lost.
    Closed,
    /// The stream failed: a transport or TLS error, the connection's end
    /// of file before either side closed the stream, a write that did not
    /// complete within its deadline, a first-level element the peer left
    /// unfinished past its deadline, a stream error either side sent other
    /// than `conflict`, or a domain peer admission no longer admits.
    /// Stanzas in flight on it may be lost in either direction.
    Lost,
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

    /// An established stream, inbound or outbound, ended as `end` says.
    /// Called once per stream, on its connection's task, after it has left
    /// the registry; it must not wait. By default nothing happens.
    fn stream_ended(&self, _stream: &VerifiedPeerStream, _end: StreamEnd) {}
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
