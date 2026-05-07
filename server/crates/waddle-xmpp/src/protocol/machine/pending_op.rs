use super::super::handlers::rich_target_validation::RichTargetKind;
use super::super::session_state::{Blocklist, CarbonsState, MucOccupancy};
use super::super::traits::HandlerId;
use jid::BareJid;

/// An async delegation the state machine is waiting to hear back about.
///
/// Emitted as part of an [`OutboundEvent`](super::super::event::OutboundEvent)
/// with a [`CallbackId`](super::super::event::CallbackId); when the
/// interpreter eventually returns an
/// [`InboundEvent`](super::super::event::InboundEvent) carrying the same
/// id, the state machine looks up the pending op, dispatches to a
/// completion handler, and drops the entry.
///
/// The variants here mirror the async outbound delegations and capture
/// whatever context the completion handler needs. Keep them small — they're
/// held in a `HashMap` on every connection.
#[derive(Debug, Clone)]
pub enum PendingOp {
    /// Awaiting a MAM window query — the original IQ id and the
    /// requester's full JID are needed to build the fin response.
    MamQuery {
        request_id: String,
        requester: jid::FullJid,
    },
    /// Awaiting link-enrichment on an outbound groupchat or direct
    /// message. The pre-enrichment stanza is retained so fallback
    /// behaviour can resend it if the enricher fails.
    Enrichment {
        fallback: Box<xmpp_parsers::message::Message>,
    },
    /// Awaiting the SFU actor's Jingle IQ response.
    Sfu {
        request_id: String,
        reply_from: Option<jid::Jid>,
        reply_to: Option<jid::Jid>,
    },
    /// Awaiting SCRAM credential load from the app's user store.
    ScramCredentials { username: String },
    /// Awaiting OAUTHBEARER token validation against `AppState`.
    OAuthBearer,
    /// Awaiting a callback that resumes a paused message-pipeline run.
    MessageDispatchResume {
        /// The handler index the pipeline paused at; resume runs the
        /// handler immediately after.
        resume_after: HandlerId,
        /// Per-completion-path payload.
        kind: ResumeKind,
        /// Connection's bound full JID at pause time, for
        /// `MessageContext` rebuild on resume.
        full_jid: jid::FullJid,
        /// Snapshot of the session-bounded state at pause time.
        blocklist: Blocklist,
        carbons: CarbonsState,
        muc_occupancy: MucOccupancy,
    },
}

/// Per-kind payload for a paused message-pipeline run.
#[derive(Debug, Clone)]
pub enum ResumeKind {
    /// The pause was triggered by `EnrichmentDispatchHandler`.
    Enrichment,
    /// The pause was triggered by `RichTargetValidationHandler`.
    RichTarget {
        kind: RichTargetKind,
        author: BareJid,
        /// Original inbound message — read by `handle_completion` to
        /// build the typed error reply.
        message: Box<xmpp_parsers::message::Message>,
    },
}
