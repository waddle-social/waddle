use jid::BareJid;
use tracing::Level;

use super::{PendingOp, ResumeKind, XmppStateMachine};
use crate::protocol::dispatch::{MessageDispatchOutcome, MessageDispatchTermination};
use crate::protocol::event::{CallbackId, OutboundEvent};
use crate::protocol::handlers::enrichment_dispatch::ENRICHMENT_CALLBACK_SENTINEL;
use crate::protocol::handlers::rich_target_validation::{
    self, RichTargetKind, RICH_TARGET_LOOKUP_CALLBACK_SENTINEL,
};
use crate::protocol::message_context::{MessageContext, MessageContextEnv};
use crate::protocol::session_state::{Blocklist, CarbonsState, Locality, MucOccupancy};
use crate::protocol::traits::HandlerId;

/// Frozen snapshot of session-bounded state at message-dispatch start.
///
/// Per #229 Q5, `MessageContext` is frozen for the duration of one
/// logical dispatch — even when the pipeline parks and re-enters via a
/// callback, the resumed handlers see the same view of blocklist /
/// carbons / occupancy as the initial dispatch did.
#[derive(Debug, Clone)]
pub(super) struct SessionStateSnapshot {
    pub(super) blocklist: Blocklist,
    pub(super) carbons: CarbonsState,
    pub(super) muc_occupancy: MucOccupancy,
}

impl XmppStateMachine {
    pub(super) fn handle_message_outcome(
        &mut self,
        outcome: MessageDispatchOutcome,
        message: xmpp_parsers::message::Message,
        full_jid: &jid::FullJid,
        snapshot: Option<&SessionStateSnapshot>,
    ) -> Vec<OutboundEvent> {
        let MessageDispatchOutcome {
            mut events,
            termination,
        } = outcome;
        match termination {
            MessageDispatchTermination::Completed | MessageDispatchTermination::Halted { .. } => {
                events
            }
            MessageDispatchTermination::Awaiting { resume_after } => {
                let id = self.next_callback_id();
                let inferred = match infer_resume_kind(&events, &message, full_jid) {
                    Some(k) => k,
                    None => {
                        events.push(OutboundEvent::Log {
                            level: Level::ERROR,
                            message: "MessageDispatchOutcome::Awaiting with no recognised \
                                      callback event; pipeline cannot resume"
                                .to_string(),
                        });
                        return events;
                    }
                };
                replace_callback_sentinels(&mut events, id, &inferred);
                let pending_kind = match inferred {
                    InferredResume::Enrichment => ResumeKind::Enrichment,
                    InferredResume::RichTarget { kind, author } => ResumeKind::RichTarget {
                        kind,
                        author,
                        message: Box::new(message),
                    },
                };
                let (blocklist, carbons, muc_occupancy) = match snapshot {
                    Some(s) => (s.blocklist.clone(), s.carbons, s.muc_occupancy.clone()),
                    None => (
                        self.blocklist.clone(),
                        self.carbons,
                        self.muc_occupancy.clone(),
                    ),
                };
                self.register_pending_op(
                    id,
                    PendingOp::MessageDispatchResume {
                        resume_after,
                        kind: pending_kind,
                        full_jid: full_jid.clone(),
                        blocklist,
                        carbons,
                        muc_occupancy,
                    },
                );
                events
            }
        }
    }

    pub(super) fn resume_message_dispatch(
        &mut self,
        message: xmpp_parsers::message::Message,
        resume_after: HandlerId,
        full_jid: jid::FullJid,
        snapshot: SessionStateSnapshot,
    ) -> Vec<OutboundEvent> {
        let mut message = message;
        let outcome = {
            let env = MessageContextEnv {
                domain: &self.domain,
                full_jid: &full_jid,
                blocklist: &snapshot.blocklist,
                carbons: snapshot.carbons,
                muc_occupancy: &snapshot.muc_occupancy,
                has_live_transport: self.has_live_transport,
                delivery_fanout: &self.delivery_fanout,
                id_gen: self.id_gen.as_ref(),
            };
            let mctx = MessageContext::derive(env, &message);
            self.dispatcher
                .resume_message(&mut message, &mctx, resume_after)
        };
        self.handle_message_outcome(outcome, message, &full_jid, Some(&snapshot))
    }
}

/// Discriminator-only equivalent of [`ResumeKind`] returned by
/// [`infer_resume_kind`]. The caller owns the inbound message and adds
/// it to the eventual [`ResumeKind::RichTarget`] payload.
#[derive(Debug)]
enum InferredResume {
    Enrichment,
    RichTarget {
        kind: RichTargetKind,
        author: BareJid,
    },
}

fn infer_resume_kind(
    events: &[OutboundEvent],
    message: &xmpp_parsers::message::Message,
    full_jid: &jid::FullJid,
) -> Option<InferredResume> {
    for event in events {
        match event {
            OutboundEvent::RequestEnrichment { .. } => {
                return Some(InferredResume::Enrichment);
            }
            OutboundEvent::LookupArchivedMessage { .. } => {
                let blocklist = Blocklist::empty();
                let muc_occupancy = MucOccupancy::empty();
                let id_gen = crate::protocol::id_gen::FixedIdGenerator(String::new());
                let detected = rich_target_validation::detect(
                    message,
                    &MessageContext {
                        domain: "",
                        full_jid,
                        locality: Locality::Sender,
                        blocklist: &blocklist,
                        carbons: CarbonsState::Disabled,
                        muc_occupancy: &muc_occupancy,
                        has_live_transport: true,
                        delivery_fanout: &[],
                        id_gen: &id_gen,
                    },
                )?;
                return Some(InferredResume::RichTarget {
                    kind: detected.kind,
                    author: detected.author,
                });
            }
            _ => {}
        }
    }
    None
}

fn replace_callback_sentinels(
    events: &mut [OutboundEvent],
    real_id: CallbackId,
    inferred: &InferredResume,
) {
    let sentinel = match inferred {
        InferredResume::Enrichment => ENRICHMENT_CALLBACK_SENTINEL,
        InferredResume::RichTarget { .. } => RICH_TARGET_LOOKUP_CALLBACK_SENTINEL,
    };
    for event in events.iter_mut() {
        match event {
            OutboundEvent::RequestEnrichment { id, .. } if *id == sentinel => *id = real_id,
            OutboundEvent::LookupArchivedMessage { id, .. } if *id == sentinel => *id = real_id,
            _ => {}
        }
    }
}
