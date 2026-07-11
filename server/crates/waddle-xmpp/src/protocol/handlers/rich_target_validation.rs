//! Rich-target validation for XEP-0308 (correction), XEP-0424
//! (retraction), XEP-0425 (moderated retraction), and XEP-0461 (reply).
//!
//! These XEPs all reference a *previously sent* message: a correction
//! `<replace id='X'/>` claims to update message X, a retraction
//! `<retract id='X'/>` claims to remove message X, and a reply
//! `<reply id='X' to='Y'/>` claims to thread under message X.
//!
//! The handler runs on the **sender pass** only — once a rich-target
//! request reaches the recipient pipeline, validation has already
//! passed sender-side, and re-running it would either spam the same
//! lookup or, worse, produce a divergent decision against a recipient
//! archive that doesn't contain the same message.
//!
//! Two-phase callback:
//!
//! 1. [`MessageHandler::handle`] inspects the message for rich-target
//!    payloads. If none are present, the handler returns
//!    `Continue(no events)`. If one IS present, it emits a
//!    [`OutboundEvent::LookupArchivedMessage`] carrying the typed
//!    [`MessageRef`] and returns
//!    `HandlerOutcome::AwaitCallback(events)`.
//! 2. [`RichTargetValidationHandler::handle_completion`] is called by
//!    the state machine when the matching
//!    `InboundEvent::ArchivedMessageLoaded` arrives. It applies the
//!    XEP-specific validation rules and either returns
//!    `Continue(events)` (resume the pipeline) or `Halt(events)` with
//!    a typed error reply.
//!
//! XEP-conformance test names use `xep_0308_*`, `xep_0424_*`, and
//! `xep_0461_*` prefixes per the #229 Q9(b) convention so
//! `cargo test xep_0424` returns every retraction-rule test.
//!
//! # Out of scope (PR3)
//!
//! - Per-XEP completion-event dispatch in the state machine
//!   (`on_archived_message_loaded`) — wired in PR4 along with handler
//!   registration. PR3 ships the handler shape and the completion
//!   logic as a free function on the type so tests exercise both
//!   halves end-to-end without going through the state machine.
//! - XEP-0425 moderator-authorisation check (the moderator-side
//!   variant of XEP-0424). The room handler chain in PR5 owns
//!   moderator authorisation; the user-side handler here only enforces
//!   author-equality on user-initiated retractions.

use super::errors::{
    bad_request_reply, item_not_found_reply, not_acceptable_reply, send_message_error,
};
use crate::mam::MamArchiveKind;
use crate::protocol::event::{ArchivedMessage, CallbackId, MessageRef, OutboundEvent};
use crate::protocol::message_context::MessageContext;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use crate::xep::{xep0308, xep0424, xep0461};
use jid::BareJid;
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};
use xmpp_parsers::message::{Message, MessageType};

/// Sentinel callback id used by handlers that emit
/// `LookupArchivedMessage`. The state machine swaps the sentinel for a
/// real allocation when registering the pending op (PR4 cutover).
pub const RICH_TARGET_LOOKUP_CALLBACK_SENTINEL: CallbackId = CallbackId(0);

/// Discriminator for which rich-target XEP a request belongs to.
///
/// Stored alongside the dispatched callback context so the completion
/// handler knows which XEP rule set to apply when the lookup returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichTargetKind {
    /// XEP-0308 Last Message Correction.
    Correction,
    /// XEP-0424 Message Retraction (user-initiated).
    Retraction,
    /// XEP-0461 Message Replies.
    Reply,
}

/// What the dispatch step detected on the inbound message.
///
/// Returned alongside the outbound events so the state machine can
/// stash the kind in its pending-op map and route the eventual
/// completion to the right rule set.
#[derive(Debug, Clone)]
pub struct DetectedRichTarget {
    /// Which XEP rule set applies.
    pub kind: RichTargetKind,
    /// Typed reference into the archive.
    pub reference: MessageRef,
    /// The originating sender's bare JID — used by
    /// [`RichTargetValidationHandler::handle_completion`] for the
    /// author-equality check.
    pub author: BareJid,
}

/// Pipeline handler that detects rich-target requests and emits the
/// archive lookup needed to validate them.
#[derive(Debug, Default, Clone, Copy)]
pub struct RichTargetValidationHandler;

impl MessageHandler for RichTargetValidationHandler {
    fn name(&self) -> &'static str {
        "xep-rich-target-validation"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        // Sender pass only.
        if !ctx.locality.is_sender() {
            return HandlerOutcome::Continue(Vec::new());
        }
        // Groupchat rich-targets reference messages in the **room's**
        // archive, not the user's. Validating them against
        // `ctx.full_jid.to_bare()` would systematically miss every
        // legitimate MUC retraction / correction / reply and respond
        // with `<item-not-found/>`. The room handler chain in PR5
        // owns groupchat rich-target validation against the room's
        // archive (XEP-0313 §5.1.3).
        if matches!(message.type_, MessageType::Groupchat) {
            return HandlerOutcome::Continue(Vec::new());
        }
        if let Err(xep0308::CorrectionError::MissingId) =
            xep0308::parse_correction_from_message(message)
        {
            let reply = bad_request_reply(
                message,
                "Message correction payload is missing the required id attribute.",
            );
            return HandlerOutcome::Halt(vec![send_message_error(reply)]);
        }
        let Some(detected) = detect(message, ctx) else {
            return HandlerOutcome::Continue(Vec::new());
        };
        HandlerOutcome::AwaitCallback(vec![OutboundEvent::LookupArchivedMessage {
            id: RICH_TARGET_LOOKUP_CALLBACK_SENTINEL,
            archive: detected.author.clone(),
            archive_kind: MamArchiveKind::Personal,
            reference: detected.reference,
        }])
    }
}

impl RichTargetValidationHandler {
    /// Apply the XEP rule set for `kind` given the result of the
    /// archive lookup.
    ///
    /// Returns `Continue` (resume pipeline) on success, `Halt` with a
    /// typed error reply on every failure mode.
    pub fn handle_completion(
        kind: RichTargetKind,
        original: &Message,
        result: Option<&ArchivedMessage>,
        author: &BareJid,
    ) -> HandlerOutcome {
        let Some(archived) = result else {
            let reply = match kind {
                // XEP-0308: a correction must reference an existing
                // message; without one, the request is invalid.
                RichTargetKind::Correction => {
                    item_not_found_reply(original, "Correction target message not found.")
                }
                // XEP-0424 §3.4: target not found.
                RichTargetKind::Retraction => {
                    item_not_found_reply(original, "Retraction target message not found.")
                }
                // XEP-0461 §3.3: target not found.
                RichTargetKind::Reply => {
                    item_not_found_reply(original, "Reply target message not found.")
                }
            };
            return HandlerOutcome::Halt(vec![send_message_error(reply)]);
        };

        match kind {
            RichTargetKind::Correction => validate_correction(original, archived, author),
            RichTargetKind::Retraction => validate_retraction(original, archived, author),
            // XEP-0461 §3.3: reply target only needs to exist.
            RichTargetKind::Reply => HandlerOutcome::Continue(Vec::new()),
        }
    }
}

/// Inspect `message` for a rich-target payload and build the typed
/// dispatch context for it. Public so handler tests and the future
/// `on_archived_message_loaded` wiring can reuse the detection logic.
pub fn detect(message: &Message, ctx: &MessageContext<'_>) -> Option<DetectedRichTarget> {
    let author = ctx.full_jid.to_bare();

    // XEP-0308 correction — references the original message's `id`
    // attribute, which clients commonly correlate with the
    // XEP-0359 origin-id.
    if let Some(correction) = xep0308::extract_correction_from_message(message) {
        return Some(DetectedRichTarget {
            kind: RichTargetKind::Correction,
            reference: MessageRef::OriginId {
                sender: jid::Jid::from(author.clone()),
                origin_id: OriginId::new(correction.replaces_id),
            },
            author,
        });
    }

    // XEP-0424 retraction — references the target message by its
    // XEP-0359 stanza-id stamped under the user's archive.
    if let Some(xep0424::RetractionKind::Request(retraction)) =
        xep0424::extract_retraction_from_message(message)
    {
        let archive_jid: jid::Jid = author.clone().into();
        return Some(DetectedRichTarget {
            kind: RichTargetKind::Retraction,
            reference: MessageRef::StanzaId {
                stanza_id: StanzaId::new(retraction.retracts_id, archive_jid),
            },
            author,
        });
    }

    // XEP-0461 reply — references the target message by stanza-id.
    if let Some(reply) = xep0461::parse_reply_from_message(message) {
        let archive_jid: jid::Jid = author.clone().into();
        return Some(DetectedRichTarget {
            kind: RichTargetKind::Reply,
            reference: MessageRef::StanzaId {
                stanza_id: StanzaId::new(reply.id.as_str(), archive_jid),
            },
            author,
        });
    }

    None
}

fn validate_correction(
    original: &Message,
    archived: &ArchivedMessage,
    author: &BareJid,
) -> HandlerOutcome {
    if !same_author(archived, author) {
        let reply =
            not_acceptable_reply(original, "Only the original sender may correct a message.");
        return HandlerOutcome::Halt(vec![send_message_error(reply)]);
    }
    HandlerOutcome::Continue(Vec::new())
}

fn validate_retraction(
    original: &Message,
    archived: &ArchivedMessage,
    author: &BareJid,
) -> HandlerOutcome {
    if !same_author(archived, author) {
        let reply =
            not_acceptable_reply(original, "Only the original sender may retract a message.");
        return HandlerOutcome::Halt(vec![send_message_error(reply)]);
    }
    if archived.tombstoned {
        // XEP-0424 §3.5 — retracting an already-tombstoned message is a
        // no-op at best; reject with bad-request to surface the bug to
        // the client.
        let reply = bad_request_reply(original, "Target message has already been retracted.");
        return HandlerOutcome::Halt(vec![send_message_error(reply)]);
    }
    HandlerOutcome::Continue(Vec::new())
}

fn same_author(archived: &ArchivedMessage, author: &BareJid) -> bool {
    archived
        .message
        .from
        .as_ref()
        .map(|from| from.to_bare() == *author)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;
