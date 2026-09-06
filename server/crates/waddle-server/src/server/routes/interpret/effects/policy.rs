use jid::BareJid;
use waddle_xmpp_core::xep0359::StanzaId;

/// The archive authority whose minted identity occurs in this effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanEffectDependency {
    /// Execute only after this pin mutation reports `Completed`.
    AfterDmPinMutation {
        pair: crate::server::routes::websocket::DmPairKey,
        target: StanzaId,
    },
    AfterRoomMembership {
        room: BareJid,
        member: BareJid,
    },
    /// Execute only for a newly recorded invite or a successful claim.
    /// `AlreadyOutstanding` and `Claimed(false)` suppress dependent delivery.
    AfterInviteLedger {
        invite: crate::server::routes::websocket::muc_invites::OutstandingInvite,
    },
    AfterArchive {
        archive: BareJid,
        minted: StanzaId,
    },
}

/// Duplicate and tombstone rules are retained until the transaction resolves identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlanSuppressionPolicy {
    /// Suppress non-sender fan-out when the canonical ingress already exists.
    SenderOnly,
    /// Preserve sender replies and idempotent fenced room mutations on duplicates.
    #[default]
    Always,
    /// Drop this effect when the request's archive row is a tombstone.
    TombstoneSwallowed,
}
