//! Context passed to every [`super::traits::MessageHandler`].
//!
//! Distinct from [`super::event::StanzaContext`] (which IQ and presence
//! handlers continue to use): the message pipeline needs richer
//! session-bounded state — XEP-0191 blocklist, XEP-0280 carbons flag,
//! XEP-0045 occupancy snapshot, an [`super::id_gen::IdGenerator`] for
//! XEP-0359 stamping, plus the derived
//! [`super::session_state::Locality`] of the local user with respect to
//! the message.
//!
//! Per Q5 of the migration design (#229), the snapshot is **frozen at
//! dispatch start**: handlers see a consistent view of session state for
//! the duration of one message dispatch even when the dispatch parks via
//! [`super::traits::HandlerOutcome::AwaitCallback`]. Any session-state
//! mutation (e.g. a XEP-0191 block-add IQ between two messages) takes
//! effect on the **next** dispatch, not retroactively on an in-flight one.

use super::id_gen::IdGenerator;
use super::session_state::{Blocklist, CarbonsState, Locality, MucOccupancy};
use jid::FullJid;
use xmpp_parsers::message::Message;

/// Read-only context handed to every message handler in a single
/// dispatch.
///
/// The struct is borrow-only — its lifetime is the dispatch call. The
/// state machine owns the underlying values; the dispatcher constructs a
/// fresh `MessageContext<'_>` per dispatch.
pub struct MessageContext<'a> {
    /// The server's own domain (e.g. `"waddle.social"`).
    pub domain: &'a str,
    /// The full JID of the connection owner — i.e. *the local user* for
    /// this state-machine instance.
    pub full_jid: &'a FullJid,
    /// The local user's role for *this* message — derived from
    /// `(full_jid, message.from, message.to)` once at dispatch start.
    pub locality: Locality,
    /// Snapshot of the local user's XEP-0191 blocklist.
    pub blocklist: &'a Blocklist,
    /// Snapshot of the local connection's XEP-0280 carbons flag.
    pub carbons: CarbonsState,
    /// Snapshot of the local connection's XEP-0045 occupancy.
    pub muc_occupancy: &'a MucOccupancy,
    /// Whether this dispatch represents a live client transport.
    pub has_live_transport: bool,
    /// Source of fresh, opaque XEP-0359 stanza-id values.
    pub id_gen: &'a dyn IdGenerator,
}

impl<'a> MessageContext<'a> {
    /// Construct a message context, deriving locality from
    /// `(full_jid, message)`.
    pub fn derive(env: MessageContextEnv<'a>, message: &Message) -> Self {
        Self {
            domain: env.domain,
            full_jid: env.full_jid,
            locality: Locality::derive(env.full_jid, message),
            blocklist: env.blocklist,
            carbons: env.carbons,
            muc_occupancy: env.muc_occupancy,
            has_live_transport: env.has_live_transport,
            id_gen: env.id_gen,
        }
    }
}

/// Caller-supplied state needed to derive a [`MessageContext`].
///
/// Split from [`MessageContext`] because the locality field is derived
/// from the message and can't be supplied directly by the caller. The
/// dispatcher takes a `MessageContextEnv` plus the message and builds the
/// final `MessageContext`.
#[derive(Clone, Copy)]
pub struct MessageContextEnv<'a> {
    /// The server's own domain.
    pub domain: &'a str,
    /// The full JID of the connection owner.
    pub full_jid: &'a FullJid,
    /// Snapshot of the local user's XEP-0191 blocklist.
    pub blocklist: &'a Blocklist,
    /// Snapshot of the local connection's XEP-0280 carbons flag.
    pub carbons: CarbonsState,
    /// Snapshot of the local connection's XEP-0045 occupancy.
    pub muc_occupancy: &'a MucOccupancy,
    /// Whether this dispatch represents a live client transport.
    pub has_live_transport: bool,
    /// Source of fresh, opaque XEP-0359 stanza-id values.
    pub id_gen: &'a dyn IdGenerator,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use xmpp_parsers::message::{Message, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    #[test]
    fn derive_populates_locality_and_borrows_state() {
        let local = full("alice@example.com/web");
        let bl = Blocklist::empty();
        let occ = MucOccupancy::empty();
        let gen = FixedIdGenerator("id-1".to_string());

        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: &local,
            blocklist: &bl,
            carbons: CarbonsState::Enabled,
            muc_occupancy: &occ,
            has_live_transport: true,
            id_gen: &gen,
        };

        let mut m = Message::new(Some("bob@example.com".parse().expect("jid")));
        m.from = Some("alice@example.com/web".parse().expect("jid"));
        m.type_ = MessageType::Chat;

        let ctx = MessageContext::derive(env, &m);
        assert_eq!(ctx.domain, "example.com");
        assert_eq!(ctx.locality, Locality::Sender);
        assert_eq!(ctx.carbons, CarbonsState::Enabled);
        assert_eq!(ctx.id_gen.fresh_stanza_id(), "id-1");
    }
}
