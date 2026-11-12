use jid::FullJid;
use tracing::{error, warn};
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::registry::DeferredPendingFlushWakeups;
use waddle_xmpp::stream_management::{SmClaimCompletion, SmFailed, SmResumed};

use super::super::cleanup::cleanup_invalidated_detached_session;
use super::super::registration::{retire_binding, retire_remote_binding, withdraw_binding};
use super::super::state::WsConnState;
use super::super::WebSocketState;

/// Finish the XEP-0198 side effects that become safe only after the resumed
/// or freshly-bound resource has been published to the connection registry.
///
/// `handle_sm_resume` claims the detached session before bind so the old
/// stream cannot be resumed twice, but detached fanout can still append
/// stanzas during the claim-to-registration handoff. This boundary completes
/// that claim under the stream registry lock, returns the typed final
/// XEP-0198 outcome. Fresh-bind retirement is completed before registry
/// publication by the registration admission barrier.
/// The finalized XEP-0198 response plus the terminal resume result to
/// record once the caller has revalidated node authority. Emitting the
/// result before that final check would count a `resumed` (or a flip's
/// failure) the client never receives when authority is revoked in the
/// post-finalization window.
pub(in crate::server::routes::websocket) struct SmFinalizationReport {
    pub(in crate::server::routes::websocket) finalization: SmRegistrationFinalization,
    pub(in crate::server::routes::websocket) resume_outcome:
        Option<waddle_xmpp::telemetry::attributes::SmResumeOutcome>,
}

pub(in crate::server::routes::websocket) async fn finalize_sm_after_registry_registration(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    outbound_receiver: Option<&waddle_xmpp::registry::SocketOutboundReceiver>,
    failed_resume_wakeups: &mut DeferredPendingFlushWakeups,
) -> SmFinalizationReport {
    if let Some(stream_id) = conn.pending_resume_stream_id.take() {
        return complete_pending_resume_claim(
            state,
            conn,
            jid,
            owner,
            stream_id,
            outbound_receiver,
            failed_resume_wakeups,
        )
        .await;
    }

    SmFinalizationReport {
        finalization: SmRegistrationFinalization::KeepExistingResponses,
        resume_outcome: None,
    }
}

pub(in crate::server::routes::websocket) enum SmRegistrationFinalization {
    KeepExistingResponses,
    ReplaceWithResumed {
        resumed: SmResumed,
        replay_after_h: u32,
    },
    ReplaceWithFailed(SmFailed),
    ReplaceWithHandledCountTooHigh {
        acknowledged: u32,
        send_count: u32,
    },
}

async fn complete_pending_resume_claim(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    stream_id: String,
    outbound_receiver: Option<&waddle_xmpp::registry::SocketOutboundReceiver>,
    failed_resume_wakeups: &mut DeferredPendingFlushWakeups,
) -> SmFinalizationReport {
    if conn.durable_sm.is_hydrating() {
        return complete_durable_resume(
            state,
            conn,
            jid,
            owner,
            &stream_id,
            outbound_receiver,
            failed_resume_wakeups,
        )
        .await;
    }
    if super::flush_ingress_checkpoint(state, &conn.sm_state, &mut conn.sm_inbound_completion)
        .await
        .is_err()
    {
        close_registered_resume_attempt(state, conn, jid, owner).await;
        return SmFinalizationReport {
            finalization: SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "internal-server-error",
            )),
            resume_outcome: Some(super::observe::SmResumeOutcome::Storage),
        };
    }
    let resume_h = conn.pending_resume_h.take();
    let completion = match resume_h {
        Some(h) => {
            state
                .deps
                .protocol
                .sm_session_registry
                .complete_claim_if_resumable(&stream_id, h)
                .await
        }
        None => {
            state
                .deps
                .protocol
                .sm_session_registry
                .complete_claim(&stream_id)
                .await
        }
    };
    let completion_reached_terminal_boundary = completion.is_ok();
    let mut resume_outcome = None;
    let finalization = match completion {
        Ok(Some(SmClaimCompletion::Resumed(mut detached))) => match resume_h {
            Some(h) => {
                detached.inbound_count = waddle_xmpp::stream_management::sequence::max_in_window(
                    detached.inbound_count,
                    conn.sm_state.inbound_count(),
                );
                conn.sm_state.restore_from_session(&detached);
                // The resume `h` acknowledges the mod-2^32 window
                // (detached.last_acked, h] — purge the pending_delivery
                // rows this SM session claimed in exactly that window
                // (review F4). Before the ack window went wrap-aware
                // these rows were swept up by the NEXT live ack's
                // numeric `<= h` delete; a windowed delete anchored at
                // the post-resume `last_acked` would skip them forever,
                // stranding them claimed until the claim-expiry janitor
                // re-released them as duplicates.
                let acked_from_exclusive = detached.last_acked;
                let expected_fence = detached.promotion_custody.captured_fence();
                conn.sm_state.acknowledge(h);
                let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone());
                match state
                    .deps
                    .protocol
                    .pending_delivery_storage
                    .delete_acked_in_window(
                        &session_id,
                        acked_from_exclusive,
                        h,
                        match expected_fence {
                            Some(fence) => {
                                waddle_xmpp::pending_delivery::storage::PendingAckAuthority::Fenced(
                                    fence,
                                )
                            }
                            None => {
                                waddle_xmpp::pending_delivery::storage::PendingAckAuthority::Unavailable
                            }
                        },
                    )
                    .await
                {
                    Ok(_) => {
                        resume_outcome = Some(super::observe::SmResumeOutcome::Resumed);
                        SmRegistrationFinalization::ReplaceWithResumed {
                            resumed: SmResumed::new(
                                stream_id.clone(),
                                conn.sm_state.inbound_count(),
                            ),
                            replay_after_h: h,
                        }
                    }
                    Err(error) => {
                        warn!(
                            session = %session_id,
                            from = acked_from_exclusive,
                            h,
                            %error,
                            failure = "storage",
                            "pending_delivery ACK deletion retained on resume; claim release must settle it"
                        );
                        if matches!(
                            &error,
                            waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner { .. }
                        ) {
                            close_registered_resume_attempt(state, conn, jid, owner).await;
                            resume_outcome = Some(super::observe::SmResumeOutcome::Storage);
                            SmRegistrationFinalization::ReplaceWithFailed(
                                SmFailed::with_condition("internal-server-error"),
                            )
                        } else {
                            resume_outcome = Some(super::observe::SmResumeOutcome::Resumed);
                            SmRegistrationFinalization::ReplaceWithResumed {
                                resumed: SmResumed::new(
                                    stream_id.clone(),
                                    conn.sm_state.inbound_count(),
                                ),
                                replay_after_h: h,
                            }
                        }
                    }
                }
            }
            None => SmRegistrationFinalization::KeepExistingResponses,
        },
        Ok(Some(SmClaimCompletion::ReplayWindowTruncated(detached))) => {
            warn!(
                stream_id = %stream_id,
                jid = %jid,
                client_h = ?resume_h,
                replay_gap_through = ?detached.replay_gap_through,
                "SM resume claim gained a replay gap before completion"
            );
            reset_resume_attempt(
                state,
                conn,
                jid,
                Some(owner),
                Some(&stream_id),
                outbound_receiver,
                failed_resume_wakeups,
            )
            .await;
            resume_outcome = Some(super::observe::SmResumeOutcome::ReplayGap);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::resume_failed(
                "resource-constraint",
                detached.inbound_count,
            ))
        }
        Ok(Some(SmClaimCompletion::HandledCountTooHigh(detached))) => {
            let acknowledged = resume_h.unwrap_or_default();
            warn!(
                stream_id = %stream_id,
                jid = %jid,
                client_h = acknowledged,
                send_count = detached.outbound_count,
                "SM resume claim completed with handled count too high"
            );
            close_registered_resume_attempt(state, conn, jid, owner).await;
            resume_outcome = Some(super::observe::SmResumeOutcome::HandledTooHigh);
            SmRegistrationFinalization::ReplaceWithHandledCountTooHigh {
                acknowledged,
                send_count: detached.outbound_count,
            }
        }
        Ok(Some(SmClaimCompletion::Expired(detached))) => {
            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim expired before completion");
            if let Err(error) = state
                .deps
                .protocol
                .sm_session_registry
                .settle_session_acknowledgements(
                    &detached,
                    state.deps.protocol.pending_delivery_storage.as_ref(),
                )
                .await
            {
                // Registry retry custody and the durable journal keep pending
                // rows protected until the expiry janitor can settle them.
                warn!(%error, "Expired resume retained pending ACK settlement");
            }
            // Completion has durably parked this queue in non-resumable
            // promotion inventory under its original claim. This snapshot
            // retires presence/occupancy only; the janitor owns promotion and
            // releases the claim after canonical source confirmation.
            let handled = waddle_xmpp::stream_management::sequence::max_in_window(
                detached.inbound_count,
                conn.sm_state.inbound_count(),
            );
            cleanup_invalidated_detached_session(state, detached, Some(owner)).await;
            close_registered_resume_attempt(state, conn, jid, owner).await;
            resume_outcome = Some(super::observe::SmResumeOutcome::NotFound);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::resume_failed(
                "item-not-found",
                handled,
            ))
        }
        Ok(None) => {
            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim disappeared before completion");
            close_registered_resume_attempt(state, conn, jid, owner).await;
            resume_outcome = Some(super::observe::SmResumeOutcome::NotFound);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "item-not-found",
            ))
        }
        Err(_error) => {
            warn!(stream_id = %stream_id, jid = %jid, failure = "storage", "Failed to complete SM resume claim");
            close_registered_resume_attempt(state, conn, jid, owner).await;
            resume_outcome = Some(super::observe::SmResumeOutcome::Storage);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "internal-server-error",
            ))
        }
    };
    if let Some(guard) = conn.pending_resume_claim.take() {
        if completion_reached_terminal_boundary {
            guard.commit();
        } else {
            drop(guard);
        }
    }
    SmFinalizationReport {
        finalization,
        resume_outcome,
    }
}

/// Complete a resume the durable authority committed, now that its endpoint
/// is registered.
///
/// The authority publishes the session as live and returns the frames
/// admitted while it hydrated. `<resumed/>` is built from the commit's
/// witness, and the caller writes it, then the replay, then that tail, before
/// any live frame. If the completion fails the session is returned to
/// detached, the registration is rolled back, and the client is told the
/// resume failed.
async fn complete_durable_resume(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    stream_id: &str,
    outbound_receiver: Option<&waddle_xmpp::registry::SocketOutboundReceiver>,
    failed_resume_wakeups: &mut DeferredPendingFlushWakeups,
) -> SmFinalizationReport {
    conn.pending_resume_h = None;
    match conn.durable_sm.complete_hydration(&mut conn.sm_state).await {
        Ok(super::durable::DurableResumed {
            resumed,
            replay_after_h,
            inline_results,
        }) => {
            // A resume requested inside a SASL2 exchange is reported in its
            // `<success/>`, which takes the place of `<resumed/>`.
            if let Some(success) = conn.sasl2_success.as_mut() {
                success.resumed(inline_results);
            }
            SmFinalizationReport {
                finalization: SmRegistrationFinalization::ReplaceWithResumed {
                    resumed,
                    replay_after_h,
                },
                resume_outcome: Some(super::observe::SmResumeOutcome::Resumed),
            }
        }
        Err(failure) => {
            warn!(stream_id, jid = %jid, ?failure, "durable SM resume was not published");
            if let Some(success) = conn.sasl2_success.as_mut() {
                success.resume_not_published();
            }
            let handled = conn.sm_state.inbound_count();
            let known = conn.durable_sm.abandon_hydration().await;
            reset_resume_attempt(
                state,
                conn,
                jid,
                Some(owner),
                Some(stream_id),
                outbound_receiver,
                failed_resume_wakeups,
            )
            .await;
            if !known {
                // Whether this binding still owns the session is unknown, so
                // the client may not bind afresh on this stream.
                conn.phase = ConnectionPhase::closing(Some(jid.clone()));
            }
            let (condition, outcome) = match failure {
                super::durable::HydrationFailure::Superseded => {
                    ("item-not-found", super::observe::SmResumeOutcome::NotFound)
                }
                super::durable::HydrationFailure::NotHydrating
                | super::durable::HydrationFailure::Unavailable => (
                    "internal-server-error",
                    super::observe::SmResumeOutcome::Storage,
                ),
            };
            SmFinalizationReport {
                finalization: SmRegistrationFinalization::ReplaceWithFailed(
                    SmFailed::resume_failed(condition, handled),
                ),
                resume_outcome: Some(outcome),
            }
        }
    }
}

/// Undo a committed durable resume whose endpoint was not published: the
/// session returns to detached and the connection to its authenticated,
/// unbound state. Returns whether there was such a resume; a connection that
/// staged none is left untouched.
pub(in crate::server::routes::websocket) async fn abandon_unpublished_resume(
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> bool {
    if !conn.durable_sm.is_hydrating() {
        return false;
    }
    let known = conn.durable_sm.abandon_hydration().await;
    if let Some(success) = conn.sasl2_success.as_mut() {
        success.resume_not_published();
    }
    conn.pending_resume_stream_id = None;
    conn.pending_resume_h = None;
    if let Some(jid) = conn.phase.bound_jid().cloned() {
        reset_resume_attempt(
            state,
            conn,
            &jid,
            None,
            None,
            None,
            &mut DeferredPendingFlushWakeups::default(),
        )
        .await;
        if !known {
            conn.phase = ConnectionPhase::closing(None);
        }
    }
    true
}

/// A known detached stream can outlive its bind authority across a Recreate
/// cutover. Fail resumption on the SM layer so the authenticated client can
/// bind afresh and retry its unhandled outbound tail on this same socket.
pub(in crate::server::routes::websocket) async fn reject_unregistered_resume(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
) -> SmRegistrationFinalization {
    let handled = conn.sm_state.inbound_count();
    // A resume the durable authority committed is returned to detached; the
    // session stays resumable for the rest of its window.
    conn.durable_sm.abandon_hydration().await;
    if let Some(success) = conn.sasl2_success.as_mut() {
        success.resume_not_published();
    }
    if let Some(claim) = conn.pending_resume_claim.take() {
        claim.release().await;
    }
    conn.pending_resume_stream_id = None;
    conn.pending_resume_h = None;
    reset_resume_attempt(
        state,
        conn,
        jid,
        None,
        None,
        None,
        &mut DeferredPendingFlushWakeups::default(),
    )
    .await;
    conn.pending_finalized_resume_outcome = Some(super::observe::SmResumeOutcome::NotFound);
    SmRegistrationFinalization::ReplaceWithFailed(SmFailed::resume_failed(
        "item-not-found",
        handled,
    ))
}

async fn reset_resume_attempt(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: Option<&waddle_xmpp::registry::LocalConnectionBindingId>,
    _failed_stream_id: Option<&str>,
    outbound_receiver: Option<&waddle_xmpp::registry::SocketOutboundReceiver>,
    failed_resume_wakeups: &mut DeferredPendingFlushWakeups,
) {
    // This rollback runs inline while the sole live WebSocket task handles the
    // client's inbound SM request. That task owns the only receiver for this
    // socket queue, and it cannot concurrently dequeue or handle another
    // outbound stanza while this function is on the stack. After sealing the
    // exact endpoint, its paired receiver proves whether any accepted frame is
    // queued. Old reservations may still hold capacity, but their commits must
    // recheck this sealed admission and cannot enqueue into a fresh binding.
    let failed_binding = conn.binding_ref.clone();
    conn.retire_binding_ref();
    let mut removed_owner = false;
    let mut recovered_endpoint = None;
    let mut exact_binding_match = false;
    if let Some(owner) = owner {
        let registry = &state.deps.protocol.connection_registry;
        let current = registry.entry_if_owner(jid, owner);
        if let Some(entry) = current {
            if let Some(binding) = entry.binding_ref().cloned() {
                if entry.seal_binding_for_failed_resume(*owner, &binding, failed_resume_wakeups) {
                    if let Some(removed) = withdraw_binding(state, jid, owner) {
                        removed_owner = true;
                        exact_binding_match = failed_binding.as_ref() == Some(&binding);
                        if exact_binding_match {
                            recovered_endpoint = match outbound_receiver {
                                Some(receiver) => removed
                                    .into_socket_outbound_endpoint_if_receiver_empty(
                                        *owner, &binding, receiver,
                                    ),
                                None => {
                                    removed.into_socket_outbound_endpoint_if_empty(*owner, &binding)
                                }
                            };
                        }
                    }
                } else {
                    error!(
                        jid = %jid,
                        "failed resume could not seal its exact outbound binding"
                    );
                }
            } else {
                error!(
                    jid = %jid,
                    "failed resume owner has no local binding to seal"
                );
            }
        }
        if removed_owner {
            if let Some(endpoint) = recovered_endpoint.take() {
                if conn.recovered_pending_endpoint.is_none()
                    && !conn.resume_endpoint_handoff_blocked
                {
                    conn.recovered_pending_endpoint = Some(endpoint);
                } else {
                    conn.resume_endpoint_handoff_blocked = true;
                    error!(
                        jid = %jid,
                        "failed resume found conflicting outbound endpoint custody"
                    );
                }
            } else {
                // Accepted frames or an unproven receiver may still belong to
                // the old binding. Keep its queue in place, but do not hand it
                // to a fresh stream attempt.
                conn.resume_endpoint_handoff_blocked = true;
                if exact_binding_match {
                    warn!(
                        jid = %jid,
                        "failed resume outbound queue was not empty; same-socket rebind is blocked"
                    );
                } else {
                    warn!(
                        jid = %jid,
                        "failed resume binding identity changed before outbound handback"
                    );
                }
            }
            #[cfg(feature = "test-support")]
            if let Some(stream_id) = _failed_stream_id {
                super::super::characterization_seams::observe_failed_resume_handoff(
                    jid,
                    stream_id,
                    conn.resume_endpoint_handoff_blocked,
                );
            }
            retire_binding(state, jid, owner).await;
            retire_remote_binding(state, jid, owner).await;
        } else {
            // Ownership moved or its exact binding could not be sealed. Do not
            // reuse any sender from this socket for a successor attempt.
            conn.resume_endpoint_handoff_blocked = true;
            warn!(
                jid = %jid,
                "failed resume lost exact registry ownership before outbound handback"
            );
        }
    }
    conn.registry_owner = None;
    super::super::account_binding::retire_current(state, conn).await;
    // The adopted detached generation is rolled back too: the detached
    // session is restored and will be invalidated with ITS generation, so a
    // fresh bind on this connection must not share it (#1703).
    if let Some(lifecycle) = conn.socket_lifecycle.take() {
        lifecycle.finish(waddle_xmpp::registry::SocketCleanupState::Detached);
        #[cfg(feature = "test-support")]
        if let (Some(owner), Some(stream_id)) = (owner, _failed_stream_id) {
            super::super::characterization_seams::delay_failed_resume_cleanup(
                state,
                jid,
                owner,
                stream_id,
                lifecycle.entry.clone(),
            );
        }
        state
            .deps
            .protocol
            .connection_registry
            .prune_completed_bind(jid);
    }
    conn.occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();
    conn.phase = ConnectionPhase::authenticated(jid);
    // Replace (never null) the per-connection state machine: the
    // connection loop's keepalive timer arm feeds `Tick` into
    // `conn.state_machine`, and the policy re-arms the adapter's timer
    // wheel on every tick. Dropping the machine to `None` here would
    // let the next tick hit the loop's no-machine guard, which cannot
    // re-arm — permanently disarming dead-peer detection (issue #1090)
    // for whatever remains of this connection. A fresh pre-bind
    // machine restores the session-free semantics this reset wants
    // (an `Unauthenticated` machine drops peer stanzas with a WARN,
    // matching the old `None` guard) while keeping the keepalive
    // clock chain unbroken.
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let keepalive = conn.keepalive_config;
    conn.init_prebind_state_machine(&domain, &state.deps.protocol.dispatcher, keepalive);
    conn.sm_ingress_fence = None;
    conn.sm_state.reset();
    // These flags belong to the failed resumed stream. The next fresh bind
    // starts unavailable until initial presence and must opt in again to
    // carbons, roster pushes, and other session-scoped subscriptions.
    conn.carbons_enabled = false;
    conn.roster_interested = false;
    conn.blocklist_interested = false;
    conn.presence_available = false;
    conn.presence_show = None;
    conn.presence_status = None;
    conn.presence_priority = 0;
    conn.presence_payloads.clear();
    conn.pending_subscribes_flushed = false;
    conn.suppress_sm_record_next_batch = false;
}

async fn close_registered_resume_attempt(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
) {
    conn.retire_binding_ref();
    let removed = withdraw_binding(state, jid, owner).is_some();
    if removed {
        // ADR-0017 Phase 1: mirror the DashMap unregister into the actor
        // tree so the resource the bind mirror-registered does not leak when
        // a resume attempt is closed out. Owner-gated on the same token so a
        // superseding newcomer is not clobbered.
        retire_binding(state, jid, owner).await;
        retire_remote_binding(state, jid, owner).await;
    }
    conn.registry_owner = None;
    super::super::account_binding::retire_current(state, conn).await;
    conn.phase = ConnectionPhase::closing(Some(jid.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state_with_sm_registry_and_pending_storage, register_test_connection,
    };
    use std::sync::Arc;
    use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{
        DetachedSessionSnapshot, InMemorySmSessionRegistry, PromotionCaptureCustody,
        SmSessionRegistry, StreamManagementState,
    };
    use waddle_xmpp::telemetry::attributes::SmEvictionPath;

    #[tokio::test]
    async fn xep0198_expired_resume_reports_handled_count_without_requeuing_acknowledged_stanza() {
        let source = Arc::new(InMemorySmPersistence::new());
        let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(source.clone()));
        let pending = Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            registry.clone(),
            pending.clone(),
        )
        .await;
        let jid: FullJid = "alice@example.com/expired-resume"
            .parse()
            .expect("recipient");
        let stream = SmSessionId::new("websocket-expired-completion");
        let mut sm = StreamManagementState::new();
        sm.enable(stream.as_str().to_owned(), true, Some(1));
        for _ in 0..3 {
            sm.increment_inbound();
        }
        let mut message = xmpp_parsers::message::Message::new(Some(jid.clone().into()));
        message.from = Some("bob@example.com/phone".parse().expect("sender"));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "unhandled message".to_owned(),
        );
        let _ = sm.record_outbound(
            waddle_xmpp::parser::stanza_to_string(message).expect("typed message XML"),
            SmEvictionPath::DirectOutbound,
        );
        let mut detached = sm
            .to_detached_session(DetachedSessionSnapshot {
                user_id: "alice".to_owned(),
                jid: jid.clone(),
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .expect("detached queue");
        detached.promotion_custody = PromotionCaptureCustody::never_claimed();
        registry.store_session(detached).await.expect("store queue");
        registry
            .claim_session(stream.as_str())
            .await
            .expect("claim")
            .expect("live session");
        let (sender, _receiver) = tokio::sync::mpsc::channel(8);
        let owner = register_test_connection(&state, &jid, sender).await;
        let mut conn = WsConnState::new();
        conn.sm_state = sm;
        // Resume restoration can recover a newer durable ingress frontier
        // than the claimed detached snapshot's count of three.
        conn.sm_state.increment_inbound();
        conn.sm_state.increment_inbound();
        conn.pending_resume_stream_id = Some(stream.as_str().to_owned());
        conn.pending_resume_h = Some(1);
        conn.pending_resume_claim = Some(super::super::SmResumeClaimGuard::new(
            registry.clone(),
            stream.as_str().to_owned(),
        ));
        conn.registry_owner = Some(owner);
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let report = finalize_sm_after_registry_registration(
            &state,
            &mut conn,
            &jid,
            &owner,
            None,
            &mut DeferredPendingFlushWakeups::default(),
        )
        .await;
        let SmRegistrationFinalization::ReplaceWithFailed(failed) = report.finalization else {
            panic!("expired resume must produce failed, never resumed or replay");
        };
        let failed = failed.to_element();
        assert!(failed.is("failed", waddle_xmpp::stream_management::SM_NS));
        assert_eq!(failed.attr("h"), Some("5"));
        assert!(failed
            .get_child("item-not-found", xmpp_parsers::ns::XMPP_STANZAS)
            .is_some());
        assert_eq!(
            report.resume_outcome,
            Some(super::super::observe::SmResumeOutcome::NotFound)
        );
        assert!(conn.phase.is_closing());
        assert!(conn.pending_resume_claim.is_none());
        assert!(conn.registry_owner.is_none());
        let canonical = source
            .get_session(&stream)
            .await
            .expect("canonical inventory")
            .expect("source retained");
        assert_eq!(canonical.max_resume_time, Some(0));
        assert_eq!(
            source
                .list_unacked(&stream)
                .await
                .expect("unacked custody")
                .len(),
            0
        );

        crate::server::session_janitors::run_sm_expiry_sweep(&state).await;
        assert!(source
            .get_session(&stream)
            .await
            .expect("canonical source settled")
            .is_none());
        assert_eq!(
            state
                .deps
                .protocol
                .pending_delivery_storage
                .list(&jid.to_bare())
                .await
                .expect("offline handoff")
                .len(),
            0
        );
        crate::server::session_janitors::run_sm_expiry_sweep(&state).await;
        assert_eq!(
            state
                .deps
                .protocol
                .pending_delivery_storage
                .list(&jid.to_bare())
                .await
                .expect("acknowledged stanza stays retired")
                .len(),
            0
        );
    }
}
