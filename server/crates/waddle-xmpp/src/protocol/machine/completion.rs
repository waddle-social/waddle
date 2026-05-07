use tracing::Level;

use super::message_pipeline::SessionStateSnapshot;
use super::{PendingOp, ResumeKind, XmppStateMachine};
use crate::protocol::event::{ArchivedMessage, CallbackId, CallbackResult, OutboundEvent};
use crate::protocol::handlers::rich_target_validation::RichTargetValidationHandler;
use crate::protocol::traits::HandlerOutcome;

impl XmppStateMachine {
    pub(super) fn on_enrichment_complete(
        &mut self,
        id: CallbackId,
        rewritten: xmpp_parsers::message::Message,
    ) -> Vec<OutboundEvent> {
        // Peek before taking — a kind-mismatch must NOT permanently
        // consume the pending op, otherwise the *correct* completion
        // arriving later would see "unknown callback id" and the
        // pipeline would silently drop.
        let pending_kind_matches = matches!(
            self.pending_ops.get(&id),
            Some(PendingOp::MessageDispatchResume {
                kind: ResumeKind::Enrichment,
                ..
            })
        );
        if !pending_kind_matches {
            return self.unmatched_completion_log(id, "EnrichmentComplete", "Enrichment");
        }
        let op = self.pending_ops.remove(&id).expect("peek succeeded above");
        match op {
            PendingOp::MessageDispatchResume {
                resume_after,
                kind: ResumeKind::Enrichment,
                full_jid,
                blocklist,
                carbons,
                muc_occupancy,
            } => self.resume_message_dispatch(
                rewritten,
                resume_after,
                full_jid,
                SessionStateSnapshot {
                    blocklist,
                    carbons,
                    muc_occupancy,
                },
            ),
            _ => unreachable!("peek matched ResumeKind::Enrichment"),
        }
    }

    pub(super) fn on_archived_message_loaded(
        &mut self,
        id: CallbackId,
        result: Option<&ArchivedMessage>,
    ) -> Vec<OutboundEvent> {
        // Peek before taking — see `on_enrichment_complete` for why
        // a kind-mismatch must not consume the pending op.
        let pending_kind_matches = matches!(
            self.pending_ops.get(&id),
            Some(PendingOp::MessageDispatchResume {
                kind: ResumeKind::RichTarget { .. },
                ..
            })
        );
        if !pending_kind_matches {
            return self.unmatched_completion_log(id, "ArchivedMessageLoaded", "RichTarget");
        }
        let op = self.pending_ops.remove(&id).expect("peek succeeded above");
        match op {
            PendingOp::MessageDispatchResume {
                resume_after,
                kind:
                    ResumeKind::RichTarget {
                        kind,
                        author,
                        message,
                    },
                full_jid,
                blocklist,
                carbons,
                muc_occupancy,
            } => {
                let completion =
                    RichTargetValidationHandler::handle_completion(kind, &message, result, &author);
                match completion {
                    HandlerOutcome::Continue(continue_events) => {
                        let mut all = continue_events;
                        let resumed = self.resume_message_dispatch(
                            *message,
                            resume_after,
                            full_jid,
                            SessionStateSnapshot {
                                blocklist,
                                carbons,
                                muc_occupancy,
                            },
                        );
                        all.extend(resumed);
                        all
                    }
                    HandlerOutcome::Halt(halt_events) => halt_events,
                    HandlerOutcome::AwaitCallback(events) => {
                        // Rich-target completion shouldn't itself park —
                        // surface as ERROR but at least forward the events so
                        // any reply reaches the wire.
                        let mut out = events;
                        out.push(OutboundEvent::Log {
                            level: Level::ERROR,
                            message: format!(
                                "RichTargetValidationHandler::handle_completion \
                                 returned AwaitCallback for {id:?}; not supported"
                            ),
                        });
                        out
                    }
                }
            }
            _ => unreachable!("peek matched ResumeKind::RichTarget"),
        }
    }

    fn unmatched_completion_log(
        &self,
        id: CallbackId,
        event_kind: &str,
        expected_resume_kind: &str,
    ) -> Vec<OutboundEvent> {
        let level = if self.pending_ops.contains_key(&id) {
            Level::ERROR
        } else {
            Level::WARN
        };
        let message = if self.pending_ops.contains_key(&id) {
            format!(
                "{event_kind} for callback id {id:?} but pending op is not \
                 {expected_resume_kind}-typed; pending op preserved for the \
                 expected completion"
            )
        } else {
            format!(
                "{event_kind} for unknown callback id {id:?}; late or duplicate \
                 completion, dropping"
            )
        };
        vec![OutboundEvent::Log { level, message }]
    }

    pub(super) fn on_sfu_response(
        &mut self,
        id: CallbackId,
        _result: CallbackResult,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "SfuResponse")
    }

    pub(super) fn on_mam_complete(
        &mut self,
        id: CallbackId,
        _result: CallbackResult,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "MamQueryComplete")
    }

    pub(super) fn on_scram_credentials(
        &mut self,
        id: CallbackId,
        _result: CallbackResult,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "ScramCredentialsLoaded")
    }

    pub(super) fn on_oauth_bearer_validated(
        &mut self,
        id: CallbackId,
        _result: CallbackResult,
    ) -> Vec<OutboundEvent> {
        // Bearer-token validation is security-sensitive; until this callback
        // has a real typed dispatch path, consume the pending op without
        // emitting diagnostics that could become part of a token-taint flow.
        let _ = self.take_pending_op(id);
        Vec::new()
    }

    fn log_completion(&mut self, id: CallbackId, kind: &str) -> Vec<OutboundEvent> {
        let op = self.take_pending_op(id);
        let matched = op.is_some();
        vec![OutboundEvent::Log {
            level: if matched { Level::DEBUG } else { Level::WARN },
            message: format!(
                "{kind} completion for {id:?} (matched pending op: {matched}); dispatch TBD"
            ),
        }]
    }
}
