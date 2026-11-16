//! Traffic between remote senders and local accounts (ADR-0020 D7; gate
//! G18).
//!
//! The router stays the only entry point for stanzas from S2S streams. It
//! hands a remote principal's message or presence for a local account, and
//! asks whether a remote principal's IQ request may reach a local resource,
//! through [`LocalAccounts`], the one typed entry the account side
//! implements; no handler written for client sessions is given a remote
//! sender. Until an account side is attached, account traffic stays
//! refused in both directions as D7's interim rule says.
//!
//! The decisions, the reachability inventory for local accounts and the
//! RFC 6121 rules each class follows are recorded in
//! `server/docs/planning/s2s-account-traffic-2047.md`.

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use super::principal::RemotePrincipal;

/// The most stanzas one received stanza may cause the router to send back
/// to its sender: the probe answers of this many resources, or one error,
/// one `subscribed`, or one `unavailable` per resource. Anything beyond is
/// dropped, so that nothing received can multiply into more traffic than
/// this.
pub const MAX_ACCOUNT_REPLIES: usize = 16;

/// The work one received stanza causes on the account side.
///
/// The router waits a bounded time for it and then moves on to the
/// sender's next stanza. When it stops waiting it abandons the work: from
/// then on the account side applies nothing more for that stanza (no
/// roster or store write, no delivery), so that an earlier stanza's effects
/// never land after a later one's from the same sender.
#[derive(Debug, Clone, Default)]
pub struct AccountWork {
    abandoned: CancellationToken,
}

impl AccountWork {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the router stopped waiting for this work. Checked before
    /// every effect, under the account's lock where one is held.
    pub fn abandoned(&self) -> bool {
        self.abandoned.is_cancelled()
    }

    /// Abandon the work: nothing more is applied for it.
    pub fn abandon(&self) {
        self.abandoned.cancel();
    }
}

/// The local account side of S2S traffic.
///
/// Every method that applies something decides the sender's admission
/// (ADR-0020 D1) from the principal's stream evidence and the S2S snapshot
/// the replica runs, immediately before it applies anything, so that work
/// queued before its domain stopped being admitted applies nothing.
#[async_trait]
pub trait LocalAccounts: Send + Sync {
    /// Whether `principal`'s domain is admitted now under the snapshot the
    /// replica runs. A stanza from a domain that is not gets nothing
    /// applied and nothing answered.
    fn admits(&self, principal: &RemotePrincipal) -> bool;

    /// Apply a message or presence from `principal` to the local account
    /// address `to` (a bare or full JID of an account of a served domain),
    /// and return the stanzas that answer it, each from a local address to
    /// the sender's domain. The router sends at most
    /// [`MAX_ACCOUNT_REPLIES`] of them, and none once it has abandoned
    /// `work`.
    async fn receive(
        &self,
        principal: &RemotePrincipal,
        to: &Jid,
        stanza: Stanza,
        work: &AccountWork,
    ) -> Vec<Stanza>;

    /// Whether an IQ request from `principal`, whose domain the router has
    /// found admitted, may reach the local resource `to` (RFC 6121
    /// §8.5.3.1): the account exists and does not block the sender, and
    /// the account shares presence with the sender or, when
    /// `room_occupant` is set, the sender is a remote room the resource
    /// occupies or is joining.
    async fn admits_request(
        &self,
        principal: &RemotePrincipal,
        to: &FullJid,
        room_occupant: bool,
    ) -> bool;

    /// Whether the local account `account` may send account traffic to
    /// remote JIDs now: it exists and has not been retired. A retired
    /// account's stream stays open until it ends; nothing it sends, and
    /// nothing the server sends on its behalf, leaves. Any failure answers
    /// `false`.
    async fn may_send(&self, account: &BareJid) -> bool;
}

/// Whether `stanza`, a local account's message or presence for a remote
/// JID, is account traffic the router may route once an account side is
/// attached. A `groupchat` message is room traffic only, and leaves only
/// for a room the sending resource has joined.
pub fn is_outbound_account_traffic(stanza: &Stanza) -> bool {
    match stanza {
        Stanza::Message(message) => message.type_ != MessageType::Groupchat,
        Stanza::Presence(_) => true,
        Stanza::Iq(_) => false,
    }
}

/// Whether `presence` is sent from an account's bare JID: a subscription
/// request or response, or a probe (RFC 6121 §3.1.2, §4.3.1).
pub fn is_account_presence(presence: &Presence) -> bool {
    matches!(
        presence.type_,
        PresenceType::Subscribe
            | PresenceType::Subscribed
            | PresenceType::Unsubscribe
            | PresenceType::Unsubscribed
            | PresenceType::Probe
    )
}

/// Whether `message` carries the DM pin marker, which only this server
/// originates for its own accounts and which never leaves for a remote
/// JID.
pub fn carries_dm_pin(message: &Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| payload.ns() == NS_WADDLE_PIN_V0)
}
