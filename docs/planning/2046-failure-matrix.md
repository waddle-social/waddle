# #2046 mandatory failure matrix: rows to tests

Each row of the umbrella's mandatory failure matrix, the tests on main that
exercise it, and what is not covered. Tests were found by reading test
names and their doc comments at this commit; none was run for this record,
and a listed test is evidence for the part of the row its name states, not
for the whole row.

The umbrella asks for deterministic barriers, real PostgreSQL role
boundaries and native multi-node WebSocket integration. The "kind" column
says which a test is:

- **2P**: two or more real server processes over one PostgreSQL
  (`tests/cluster_support`);
- **1P**: one real server process over WebSocket;
- **PG**: in-process against real PostgreSQL;
- **U**: in-process with deterministic barriers or seams, no database
  dependency stated.

Status: **covered** (every clause of the row has a test of the required
kind), **partial** (the stated clauses do not), **not met** (a test pins
that the server does not satisfy the row yet), **blocked** (needs a feature
that is not on main).

Binaries are in `server/crates/waddle-server/tests/` unless a path is
given. `lib` is the `waddle-server` library test crate, which CI builds and
most machines cannot.

| # | Scenario | Tests | Kind | Status | Not covered |
|---|---|---|---|---|---|
| 1 | Same full JID binds concurrently on two nodes | `clustering_cluster_e2e::failure_matrix::same_full_jid_bound_concurrently_on_two_nodes_leaves_one_binding` (added with this record, not yet run) | 2P | partial | Late register, unregister, presence, carbons and force-close against the successor are covered in-process only |
| | | `characterization_bind::concurrent_same_full_jid_binds_leave_one_live_session_and_conflict_close_the_other` and its `_under_the_actor_host` twin; `rebind_of_live_full_jid_closes_incumbent_with_conflict_and_routes_to_newcomer` | 1P | | |
| | | `lib` `websocket::tests::registration_remote_retirement::cross_node_fresh_bind_retires_socket_blocked_in_flush`, `..._blocked_in_readiness`; `waddle-xmpp` `ownership::account_resource_transition::same_full_jid_replacement_fences_late_register_unregister_and_teardown` | U | | |
| 2 | Simultaneous resume; cancellation after claim acquisition | `xep0198_cross_node_resume::pure_two_node_detached_steal_race_exactly_one_winner`, `two_simultaneous_live_resume_race_loser_falls_back_to_detached_path_and_fails_cleanly`, `reaper_wins_mid_resume_interleaving_resume_fails_cleanly`, `post_win_hydrate_failure_repairs_and_client_retry_recovers_the_session`, `resume_retransmit_race_against_the_same_new_node_dedups_without_double_hydration` | PG | partial | The races run two registries in one process, not two server processes. Cancellation of the resuming request after the claim is acquired has no test named for it |
| | | `clustering_cluster_e2e::cross_node_resume_live_steal_handshake` | 2P | | |
| 3 | Lost reply after room or SQL acceptance | `xep0045_xep0198_remote_acceptance::reply_loss_recovers_same_acceptance` | 2P | covered | |
| | | `physical_attestation` `direct_admission::postgres_committed_lost_reply_requires_fresh_authoritative_lookup`; `lib` `route_bridge::tests::diversion_recovery::committed_envelope_with_lost_reply_does_not_replay_after_cooldown`; `ingress_receipt_custody::lost_ack_retries_exact_insert`, `postgres_uncertain_ack_retries_only_receipt` | PG, U | | |
| 4 | Origin or owner death around remote acceptance and SM `h` | `xep0045_xep0198_remote_acceptance::origin_sigkill_after_owner_commit` | 2P | not met | `owner_sigkill_before_commit` is `#[ignore]`d for #1948; `owner_sigkill_before_commit_characterizes_current_silent_loss` pins that the origin's `h` covers a stanza the dead owner never accepted |
| 5 | Pending remote-room acceptance followed by later stanzas, acknowledgement requests and force-detach | `xep0045_xep0198_remote_acceptance::h_while_owner_commit_paused_characterizes_current_early_ack` | 2P | not met | `h_does_not_advance_while_owner_commit_paused` is `#[ignore]`d for #1948. Later stanzas and force-detach during a pending acceptance, and a late completion against a replacement generation, need the durable mailbox (M2) |
| 6 | Partitioned old owner continues work after takeover | `clustering_cluster_e2e::deposed_owner_with_live_socket_room_actor_scenario`, `lone_survivor_and_isolation_fencing` | 2P | partial | The transport-attempt linearization point and the classification of already-authorized bytes are defined for the supervised pumps, which production does not select yet |
| | | `socket_admission::stale_identity_never_reauthorizes_a_parked_data_frame`, `revocation_fences_data_allows_terminal_and_forces_idle_pumps_at_deadline`; `ingress_receipt_custody::postgres_fatal_fence_stops_retained_insert`, `fatal_fence_stops_retry_after_simulated_lost_ack`; `clustering_shutdown_deadline::fatal_fence_never_retires_after_socket_drain` | U, PG | | |
| 7 | Join, history, subject and live fanout races; ban or revision change; room recreation | `characterization_resume_muc::muc_join_burst_orders_presence_self_presence_subject_before_live_without_history`; `xep0045_join_lifecycle_ws`, `xep0045_admin_ordering_ws`, `xep0045_config_ordering_ws` (whole binaries) | 1P | partial | No test races a join against live fanout across two nodes while the room is recreated |
| | | `lib` `muc_durable::tombstoned_terminal_effect_blocks_recreation_without_destroy_completion_until_completed`, `pending_destroy_releases_its_claim_before_a_late_create_can_commit`; `lib` `handlers::iq::muc_admin::committed_ban_recovery_replays_presence_and_removal_side_effects` | PG, U | | |
| | | `clustering_cluster_e2e::muc_join_routes_to_foreign_room_owner`, `groupchat_fanout_reaches_foreign_node_occupant_and_terminalizes`; `restricted_clustered_startup::two_restricted_nodes_form_a_cluster_and_serve_without_ddl` (takeover, ban, destroy) | 2P | | |
| 8 | Full mailbox, blocked writer, exhausted database pool, slow room | `canonical_actor_bridge::full_actor_mailbox_and_inbound_budget_do_not_block_independent_stop`, `independent_stop_survives_full_mailbox_and_a_stalled_physical_writer`, `full_websocket_output_queue_stops_after_releasing_lifecycle_gate`; `websocket_admission_live::full_actor_mailbox_does_not_delay_shutdown`, `full_actor_mailbox_does_not_delay_permit_revocation` | U, 1P | partial | No recorded budget for unrelated users under a slow room; the workload budgets of package A are not fixed |
| | | `postgres_admission_cleanup::stuck_return_is_bounded_and_closes_the_checkout_before_reusing_admission`, `cancelled_query_and_commit_hold_the_lane_until_cleanup`; `ingress_receipt_custody::postgres_blocked_receipt_does_not_stall_a_healthy_key`, `capacity_rejection_leaves_effect_and_frontier_uncertain`; `liveness_readiness_split::postgres_outage_leaves_server_live_and_unready_then_recovers` | PG, 1P | | |
| 9 | Crash before or after SM append, transport attempt, write completion, client ACK | `graceful_shutdown_ws::sigterm_promotes_live_sm_session_unacked_queue_for_next_startup_delivery`; `characterization_resume_muc::sm_resume_after_transport_loss_reports_inbound_h_and_replays_unacked_tail`, `sm_detach_keys_unacked_ingress_frame_when_canonical_read_succeeds` | 1P | partial | Process kills at each of the four points are not driven against one server: the SIGKILL tests are the remote-acceptance ones in row 4. Durable SM custody before acceptance is M3 |
| | | `physical_continuation::aborted_post_attempt_write_retains_uncertain_receipt`, `fast_ack_during_start_send_waits_for_the_actual_receipt`; `output_continuation::uncertain_attempt_retains_frame_and_credits_and_forbids_the_suffix`; `xep0198_replay_bytes`, `xep0198_resume_receipts_ws`, `xep0198_pending_delivery_ws` (whole binaries) | U, PG, 1P | | |
| 10 | Lost relay hint, relay restart, stale cached route, account owner movement | `clustering_cluster_e2e::partial_room_fanout_completes_on_retransmission_after_relay_recovery`, `cluster_exit_criteria_end_to_end` (relay crash and respawn, node churn) | 2P | partial | A stale cached route after account-owner movement has no multi-process test |
| | | `xep0198_cross_node_resume::recipient_claim_move_retry_dedups_pending_delivery_delete`, `handoff_fanout_survives_a_cross_node_steal_unchanged`; `clustering_ordered_relay::receiver_replays_duplicate_ack_across_asserted_node_changes`, `receiver_duplicate_pending_envelope_does_not_reserve_second_effect` | PG, U | | |
| 11 | Actor or pump panic; graceful drain and deadline expiry | `canonical_actor_bridge::actor_panic_preserves_cleanup_pending_until_fenced_writer_returns`, `panic_after_pump_handoff_registers_and_joins_no_actor_cleanup`; `websocket_admission_live::killing_the_idle_hosting_actor_fences_as_an_owner_panic`, `actor_host_panic_during_settlement_fences_and_releases_the_slot`, `owner_panic_fences_before_releasing_drain_guard` | U, 1P | partial | Run under the test-selected actor host; production's host is the plain task. Orphan-socket and orphan-claim checks after a panic are per test, not a single inventory |
| | | `clustering_shutdown_deadline::sigterm_with_stalled_stages_leaves_identity_to_expiry`, `fatal_publication_wait_started_before_sigterm_outlasts_the_enclosing_deadline`; `graceful_shutdown_ws::sigterm_notifies_every_live_session`; `supervised_io` (whole binary) | U, 1P | | |
| 12 | Retention or GC during pending delivery, detach or migration | `lib` `ingress_substrate::tests::orphan_gc_retains_pending_custody`, `ingress_substrate::authority_tests::gc_rechecks_pending_intents_under_canonical_lock`, `ingress::gc_send_attempt_tests::retention_preserves_unresolved_and_collects_settled_attempts` | PG | partial | GC racing a session migration between two nodes has no test |
| | | `restricted_runtime_startup::alias_gc_collects_an_expired_message_under_the_runtime_role`; `xep0198_promotion_inventory` (whole binary) | PG | | |
| 13 | Authorization or blocking changes during queued work | `lib` `pending_delivery::tests::flush_drops_pending_row_when_sender_blocked_after_intake`, `ingress::recovery_executor_tests::blocked_direct_recovery`; `xep0191_ingress_recovery` (whole binary); `notification_outbox` `suppression::xep0191_blocked_t1_suppression_keeps_pending_delivery_intact` | PG | partial | `xep0045_xep0198_remote_acceptance::membership_change_between_acceptance_and_ack` is `#[ignore]`d ("requires remote child settlement"). Revalidation of work queued in a connection actor mailbox needs M3 |

## What was added

Row 1 had no test with two real server processes. The harness could drive
it, so
`tests/failure_matrix/mod.rs` (a module of the `clustering_cluster_e2e`
binary) binds one full JID on both nodes at once and requires that exactly
one socket keeps it, that a message sent from each node afterwards reaches
that socket once, and that the retired socket receives neither. It
compiles; it has not been run.

No test was added for the other partial rows:

- Rows 4, 5 and 13 are pinned by `#[ignore]`d contract tests whose
  behaviour arrives with #1948 and remote child settlement.
- Rows 2, 6, 9, 10 and 12 need either a seam that stops a server process
  at a named point (the acceptance seams cover the remote-room owner only)
  or the durable SM and mailbox work in flight.
- Rows 7, 8 and 11 need budgets and a host selection that package A and M3
  decide.
