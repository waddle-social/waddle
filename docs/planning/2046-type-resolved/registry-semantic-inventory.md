# Connection registry semantic inventory shard

Source review: `718f47a9e08f29cf2e6157803770e2f495fe201e` (source-tree base
`5763923e`; the later planning commit has the same tree). This is a **bounded declaration and
selected-edge review**, not an every-caller read/write inventory or evidence
that a migration phase is complete. The tables classify current operations;
they do not prescribe a future owner. **A** means authority for the named
current decision, **I** a derived index or published copy, **T** transport or
lifecycle plumbing, and **O** migration/mirror glue. Multiple labels describe
multiple roles; O does not mean a live path is safe to delete.

The marker [manifest](targets.tsv) contains 95 registry targets (one type,
eight fields including one `test-utils` probe, and 86 inherent methods) and
72 entry targets (one type, 22 fields, and 49 inherent methods). The method
counts include private and gated declarations. The groups below enumerate
those declarations; they do **not** assert that each caller has been manually
classified. Trait `Default`/`Debug` implementations and helper types are
mentioned where relevant but are not included in these counts.

The historical compiler passes at `3ae674c121c06b44934c915ecc0ef469dca54833`
reported 423 default and 483 clustering registry spans, and 401 default and
471 clustering entry spans. Those are profile-specific diagnostic rows, not
unique callers or read/write counts; the same site may occur in both profiles.
All were left `manual-review-required` in the [compiler record](README.md).
They are triage inputs, not a compiler result for this revision: the registry
module has since added a dark custody module and source lines shifted; actor
endpoint/custody files and other server sources also changed. The shard reviewer ran no Cargo command; the refreshed compiler captures were
produced separately. The unresolved rows, unmarked indirect calls, and
feature combinations remain open.

## Fields and shared state

`ConnectionEntry` is cloned into registry and actor maps; its `Arc`-backed
fields generally share state across those clones, rather than making a fresh
per-map value. A separately constructed remote entry is a separate copy. The
local binding ID is process-local equality evidence, not a node lease or a
durable resource-ownership fence ([definitions and construction](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L283)).

| Declaration group | Class | Operation and current meaning |
| --- | --- | --- |
| Registry [`bind_slots`](../../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L72) | A/T | Per-full-JID lock and lifecycle/retirement inventory serializes local bind and cleanup. `bind_pending_probes` is test instrumentation under `test-utils`, not serving state. |
| Registry [`connections`](../../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L76) | I/A/T | Full-JID route index of local or remote-hosted entries. Its current entry and binding token gate many local lookups, metadata updates, and enqueue/retirement decisions. It is not by itself durable account ownership. |
| Registry [`pending_subscription_stanzas`, `presence_states`, `last_activity`, `started_at`](../../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L78) | A/I | Process-local subscription queue and last-activity/uptime data supply current reads; `presence_states` is a full presence snapshot used for probe responses, distinct from entry availability/priority and also mirrored in UserActor. |
| Registry [`sm_stream_owners`](../../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L90) | I | Best-effort stream-ID-to-JID reverse index; writers prune/update it under the route slot's guard. A reader must recheck the entry's stream ID and hosting before acting. |
| Entry [`binding_id`, `connection_ref`, `binding_ref`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L303) | A/T/I | Local owner equality, physical connection identity, and local bind-attempt identity respectively. A remote mirror retains the source `ConnectionRef`, mints its own local token, and has no `BindingRef`. |
| Entry [`binding_permit`, `local_io_relinquishment`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L309) | A/T | Shared revocable pre-write permission and optional evidence supplied by the physical socket owner. A remote mirror has no local I/O witness. |
| Entry [`archive_dispatch`, `terminal_ordering_retry`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L312) | A/T | Per-entry ordered archive frontier and one-at-a-time terminal-retry lease. They coordinate entry-local dispatch/retry, not durable account ownership. |
| Entry [`hosting`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L314) | A/I | Local/remote classification governs whether an operation may target this node's socket; remote entries are proxies. |
| Entry [`resume_control`, `endpoint`, `force_detach_tx`, `force_detach_rx`, `write_interruption`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L315) | T/A | Shared output endpoint, once-wired resume control, take-once force-detach receiver and per-binding I/O interruption. A successful offer or queue admission alone is not cleanup or socket-write completion. |
| Entry [`carbons_enabled`, `presence_available`, `presence_priority`, `roster_interested`, `blocklist_interested`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L321) | A/I | Shared atomics drive current selection and fanout; local handlers mutate owner-gated variants while remote-owner publication seeds a separate mirrored copy. |
| Entry [`offline_flushed`, `pending_subscribes_flushed`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L331) | A | Once-per-entry CAS flags control session-scoped recovery/subscription work; `offline_flushed` may be reset on a deferred attempt. |
| Entry [`sm_stream_id`, `occupancy_session`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L344) | A/I | Shared per-entry session/generation values used by recovery, reverse-index verification and cleanup. They do not alone establish durable session or account ownership. |

## Registry method groups

The listed names are the 86 `ConnectionRegistry` inherent methods in the
manifest, grouped by the operation implemented in the linked source. Names
marked test-only below are available in tests and/or `test-utils`; an ungated
method being compiled does not prove a production caller or runtime selection.

| Methods and definition | Class | Operation / boundary |
| --- | --- | --- |
| [`new`, `pending_subscription_count`, `presence_state_count`, `last_activity_count`](../../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L94) | A/I | Initialize the process-local structures; count their snapshots for state inventory. `Default` delegates to `new`, and `Debug` reports connection count. |
| [`remove_sm_stream_owner`, `remove_retired_presence`, `set_sm_stream_id`, `set_sm_stream_id_if_owner`, `sm_stream_owner`, `set_occupancy_session_if_owner`, `occupancy_session_of`](../../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L114) | A/I | Update/read entry metadata and reverse index; private removal helpers prune while holding the slot guard. `set_sm_stream_id` is ungated by owner; its owner-qualified sibling rejects replacements. `sm_stream_owner` returns only a snapshot. |
| [`try_lock_bind`, `lock_bind`, `probe_socket_lifecycle`, `prune_completed_bind`, `observe_next_pending_bind`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L184) | A/T | Acquire a per-JID binding gate, observe lifecycle state, and prune completed slots. The contention probe is `test-utils` only; guards also carry incumbent, generation and actor-retirement state. |
| [`publish_provisional_if_expected`, `activate_provisional_if_confirmed`, `retire_provisional_if_exact`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L10) | A/T | Publish, activate, or compensate an exact candidate without removing a successor. Publication and activation are gated by `any(test, feature = "test-utils")`; retirement is ungated, but these declarations alone do not establish a serving actor cutover. |
| [`register`, `register_with_carbons`, `register_with_stream_state`, `register_entry`, `register_socket_entry`, `register_entry_if_owner_or_absent`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L110) | I/A/T | Construct or publish entries; socket publication is ordered with its interruption gate; owner-or-absent rejects a different owner. Replacements revoke old permits, handle actor admission, and maintain the SM index as implemented by each variant. |
| [`unregister`, `unregister_if_owner`, `unregister_if_sm_stream_id`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L300) | I/A/T | Remove unconditionally, by exact local token, or by matching SM ID; retirement revokes the permit and prunes SM/presence under the slot guard. |
| [`is_owned_by`, `entry_if_owner`, `local_binding_permit_if_owner`, `local_sm_enable_binding_permit_if_owner`, `is_connected`, `is_locally_hosted`, `entry`, `connection_count`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L319) | A/I/T | Query or clone the current entry; the permit variants additionally check local hosting, admission, exact binding and, for SM enable, the socket's control. The source describes `is_connected` as introspection retained after its former selection caller was retired; zero historical marker hits do not prove no current indirect use. |
| [`update_presence`, `update_presence_if_owner`, `update_presence_state`, `update_presence_state_if_owner`, `presence_state`, `clear_presence_state`, `clear_presence_state_if_owner`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L7) | A/I | Write/read availability and full presence snapshots. Guarded writes hold the connection guard across the check and update; the unqualified variants do not perform that owner check. |
| [`record_last_activity`, `last_activity`, `clear_last_activity`, `server_uptime_seconds`, `available_resources_for_user`, `cleanup_stale`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L154) | A/I/T | Mutate/read offline activity and uptime; enumerate available resources; scan closed endpoints and unregister by JID. `cleanup_stale` uses an unqualified unregister after the scan, so it should not be conflated with exact-owner cleanup. |
| [`mark_roster_interested`, `mark_roster_interested_if_owner`, `is_roster_interested`, `roster_interested_resources_for_user`, `mark_blocklist_interested`, `mark_blocklist_interested_if_owner`, `is_blocklist_interested`, `blocklist_interested_resources_for_user`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L6) | A/I | Write/read and enumerate feature-interest flags. The two unqualified `mark_*` setters are gated by `any(test, feature = "test-utils")`; the owner-qualified setters are not. |
| [`send_to_many`, `list_connections`, `active_sm_stream_ids`, `other_resources_for_user`, `other_carbon_resources_for_user`, `is_carbons_enabled`, `set_carbons_enabled`, `set_carbons_enabled_if_owner`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L108) | I/A/T | Enumerate route/SM/carbon snapshots, update carbons, or fan out using `send_to`. The unqualified carbons setter is gated by `any(test, feature = "test-utils")`. |
| [`reserve_pending_flush`, `try_reserve_pending_flush`, `send_reserved_pending_flush`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L143) | A/T | Capture owner, binding and endpoint before waiting or reserving; synchronously recheck eligibility and commit. `AcceptedOrUnknown` is not a proof of write, durable custody, or definite non-enqueue. |
| [`send_to_with_write_acceptance`, `send_to`, `send_to_if_owner`, `send_pending_flush`, `send_pending_flush_if_owner`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L240) | T/A | Async admission. `send_to` may retry a replacement only after a proven closed old endpoint; the owner-qualified variant does not retarget. The acceptance-aware path rechecks the captured endpoint after waiting; its token is resolved later by the socket write/recovery owner. |
| [`try_send_to`, `try_send_outbound_to_locally_hosted`, `try_send_to_matching`, `try_send_outbound_if_owner`, `capture_owned_enqueue_if_owner`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L508) | T/A | Nonblocking matching/admission or a captured exact-owner enqueue closure; closed/full/provisional/sealed results have different meanings. Local-hosted admission rejects a remote mirror. |
| [`remove_if_sender_closed`, `remove_if_sender_closed_owner`, `remove_closed_owner_entry`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L701) | I/T | Evict closed endpoints; owner/endpoint matching in the relevant variants prevents an old failure from removing a replacement. |
| [`queue_pending_subscription_stanza`, `remove_pending_subscribe`, `drain_pending_subscription_stanzas`, `pending_subscription_stanzas`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/subscriptions.rs#L3) | A | Queue/deduplicate subscribe requests, remove or drain them, or read without draining for initial-presence delivery. |
| [`local_sm_stream`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L194) | A/I | Read a locally hosted entry's SM stream snapshot for archive dispatch; it is not an independent durable ownership check. |

## Entry method groups

These groups enumerate the 49 marked `ConnectionEntry` inherent methods;
private helpers are included. `ProvisionalConnectionEntry` and endpoint,
permit, guard and lifecycle methods are separate types and are not counted.

| Methods and definition | Class | Operation / boundary |
| --- | --- | --- |
| [`new`, `new_for_connection`, `new_for_binding`, `new_for_binding_with_local_io`, `new_provisional_for_binding`, `new_actor_provisional_for_binding`, `new_entry`, `remote_hosted`, `remote_hosted_for_connection`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L403) | A/T/I | Construct active local, non-admitting candidate, or remote-hosted entry; `new_entry` mints local identity and shared state. Remote construction deliberately does not invent a source binding attempt. Candidate constructors alone do not select the candidate for `/ws`. |
| [`wire_resume_control`, `resume_control`, `activate_binding`, `seal_binding`, `seal_actor_binding_if_any`, `same_exact_local_binding`, `actor_custody`, `is_provisional`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L529) | A/T | Wire/read socket control; activate/seal or compare exact binding/admission. `activate_binding` and `is_provisional` are gated by `any(test, feature = "test-utils")`. |
| [`into_socket_outbound_endpoint_if_empty`, `into_socket_outbound_endpoint_if_receiver_empty`, `connection_ref`, `binding_ref`, `local_io_relinquishment`, `outbound_queue_is_empty_for_test`, `outbound_queue_is_closed_for_test`, `is_locally_hosted`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L627) | A/T/I | Reclaim a sealed exact socket endpoint under the documented emptiness constraints, or inspect identity/hosting/witness. Both queue observations are `test-utils` only; receiver-empty reclaim also requires no handler in flight. |
| [`try_acquire_terminal_ordering_retry`, `force_detach_sender`, `try_force_detach_for_jid`, `try_force_detach_raw`, `try_force_detach_and_interrupt`, `try_force_detach_and_interrupt_after_offer`, `try_force_detach_and_revoke`, `try_force_detach_and_revoke_after_reserve`, `try_force_detach_and_revoke_after_reserve_for_test`, `try_force_detach_and_interrupt_after_offer_for_test`, `take_force_detach_rx`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L697) | A/T | Bound retry work; offer or reserve control, revoke/interrupt under the applicable exact-binding rules, and take the receiver once. The two `*_for_test` race hooks require `any(test, feature = "test-utils")`; enqueue/reservation is not detached or persisted completion. |
| [`binding_id`, `is_carbons_enabled`, `is_presence_available`, `presence_priority`, `claim_offline_flush`, `claim_pending_subscribes_flush`, `reset_offline_flush`, `occupancy_session`, `set_sm_stream_id`, `sm_stream_id`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L880) | A/I | Read identities and shared state, perform session CAS transitions, and read/write SM ID. Registry setters also maintain the reverse index; the entry setter by itself does not. |
| [`try_send_archive_ordered`, `try_send_archive_ordered_with_refusal`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L74) | A/T | Coordinate endpoint enqueue with the per-entry archive frontier. The refusal variant preserves the original envelope on proven pre-enqueue rejection. |
| [`commit_pending_flush`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L103) | A/T | Private synchronous exact-owner, binding and endpoint check before committing a previously reserved permit. |

## Selected production and indirect edges

These links document reviewed examples and indirection, not an exhaustive
caller list. The compiler data is reference evidence at its own baseline;
these descriptions are source observations at the review revision.

| Edge | Class | Producer, operation, and consequence |
| --- | --- | --- |
| Socket bind and rollback | A/I/T/O | Live [`/ws` creates the socket endpoint](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L454); [registration](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L640) creates a binding-backed entry, publishes the socket entry and lifecycle, records occupancy, then waits for actor/remote registration. [Rollback](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L238) uses owner-scoped cleanup. [`mirror_register_outcome` / `mirror_unregister`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L109) are active bridge glue; actor confirmation and cleanup are not optional merely because the target architecture replaces dual registration. |
| Bare-JID selection / actor clone | A/I/T | [`UserActor`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L22) holds shared entry clones and [selects](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L185) available resources at the highest nonnegative priority. Its [delivery method](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L53) uses the entry's archive-ordered endpoint directly, an indirect path that does not call registry `send_to`. [`UserEntry.resources`](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L70) is another clone map for hard demotion. |
| Direct and ordered route | A/I/T | [`deliver_direct_to_full_locally`](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1408) creates an envelope and calls local-hosted registry admission. [`deliver_ordered_local_copy`](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1446) captures an entry before asynchronous readiness work and later captures owner-checked enqueue. Selection, channel admission, and writer acceptance are distinct boundaries. |
| Pending delivery | A/I/T | [`flush_for_resource`](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L185) checks the owner/hosting/presence and resets a deferred flush; [row dispatch](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L619) reserves or sends with a captured owner. A pending row and its acknowledgment semantics are outside the registry's in-memory queue. |
| Presence, interests and subscriptions | A/I | The [presence handler](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L60) owner-gates updates and once-per-entry subscription flush. [Subscription handling](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/subscription/implementation.rs#L260) queues requests; [roster](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/roster/implementation.rs#L111), [blocklist](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/blocking.rs#L67), and [carbons](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/sans_io.rs#L178) mutate their owner-qualified flags. |
| Durable-effect acceptance | T/A | [Room-effect drain](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L594) uses write-acceptance admission. The [WebSocket writer](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L180) acknowledges after its stated write/recovery boundary, not merely after registry enqueue. |
| Same-node and cross-node resume | I/A/T | [Validated local resume](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L47) rechecks reverse-index result against local hosting, the entry's SM ID and binding/control before the owner handles it. [Cross-node forced detach](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L158) similarly rechecks the best-effort index; timeout and queue offer are not cleanup proof. The cross-node bridge is gated by clustering, while the validated local function is not. |
| Remote owner mirror | O/I/T/A | Under clustering, the [owner](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L369) constructs a remote-hosted entry from the source physical reference, seeds state, registers its clone with UserActor and the owner registry, and starts a forwarder after guards. [Snapshot application](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L419) directly writes entry fields and presence; it is an indirect mutation outside registry methods. [The forwarder](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket_forwarder.rs#L20) holds an envelope while relaying; [socket delivery](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L558) uses the registered owner token for local admission. This is active routing glue, not the target resource authority. |
| Cluster demotion / lifecycle | A/I/T | [`UserLocalClaims`](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L788) consults registry entries for demotion; [`terminal_sweep`](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L957) also enumerates sockets. The ordinary `owned` reconcile deliberately uses the user registry and does not fall back to raw sockets. Cleanup includes paths not represented by direct send calls. |

## Feature gates, tests, and remaining work

- The [server manifest](../../../server/crates/waddle-server/Cargo.toml#L159) enables XMPP `test-utils` through dev-dependencies and `test-support`, not ordinary library dependencies. `clustering` compiles additional remote machinery; its runtime configuration is a separate condition. The marker passes used default and clustering without `--tests`; they do not cover test targets or every feature combination.
- Registry [`tests.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/tests.rs#L346) defines replacement, owner-checked send, reservation, metadata, SM-index and presence cases; [`binding_permit_tests.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding_permit_tests.rs#L96) defines permit/replacement and force-detach races. [Remote owner tests](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L1402) cover replacement during actor admission. These are source-defined tests, not passing results for this revision.
- The [P3 edge review](P3-edge-review.md) separately examines selected outbound producers, queues, acceptance and the actor candidate at its stated older revision. Its findings must be reconciled with current custody changes before reuse as current evidence.
- Still unreviewed: other target families outside this registry/entry export, unmarked aliases and trait or macro expansion edges, all call paths of helper and guard types, direct field users not captured by the marker pass, current actor-local custody changes, and test-only or other feature combinations. No count here claims those sites have been reviewed. Independent review and the full #2046 caller and authority checklist remain open.

## Per-reference review ledger

This ledger classifies **739 of 955 distinct historical reference identities**
(and leaves **216 unreviewed**). A reference identity is a marker tag, file,
line, column, and compiled target; the same identity appearing in both profiles
is counted once. These 739 identities account for 626 default and 738 clustering
profile rows. The denominator is the union of the registry and entry families
in both historical TSVs, not a fresh current-revision compiler run. The twenty-eight
ledger files were byte-identical to the marker baseline when checked; other
files, notably the registry module and actor endpoint, have changed and their
coordinates require reconciliation. Each row names the exact old diagnostic
coordinate and marked declaration. A type-only reference has no read/write
operation at that span, even if the surrounding function later acts on it.
Runtime reachability is not established by a compiled reference.

The [structured site reviews](registry-site-reviews.tsv) reconcile the
historical ledger and add individually reviewed references at current-source
coordinates. They contain **1,778 refreshed profile rows**: 824 of 824
default and 954 of 954 clustering registry/entry references. The export includes 297
type-only profile rows, explicitly excluded from runtime-operation
classification. Every export key and source line was matched against the
refreshed diagnostics; this registry-only export cannot satisfy the
full-inventory validator by itself. Unmarked, indirect, other-feature and runtime
coverage remain open.

| Reference site | Marked target | Class | Reviewed operation at this site |
| --- | --- | --- | --- |
| [resume_bridge.rs:34:28](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L34) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [resume_bridge.rs:48:16](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L48) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [resume_bridge.rs:54:30](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L54) | `CR::sm_stream_owner` | I | read best-effort SM reverse index to find a candidate JID. |
| [resume_bridge.rs:57:32](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L57) | `CR::entry` | I | read and clone the candidate's current route entry. |
| [resume_bridge.rs:60:15](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L60) | `CE::is_locally_hosted` | A/I | read hosting to reject a nonlocal candidate. |
| [resume_bridge.rs:60:44](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L60) | `CE::sm_stream_id` | A/I | read entry SM ID and reject a stale reverse-index hit. |
| [resume_bridge.rs:66:35](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L66) | `CE::binding_ref` | A/T | read and clone the exact local binding attempt. |
| [resume_bridge.rs:69:31](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L69) | `CE::resume_control` | T | read the owner-serviced resume-control handle. |
| [resume_bridge.rs:113:39](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L113) | `CRF::TYPE` | — | bridge field type; no runtime action at this span. |
| [resume_bridge.rs:128:38](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L128) | `CRF::TYPE` | — | wire parameter type; no runtime action at this span. |
| [resume_bridge.rs:158:16](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L158) | `CRF::TYPE` | — | force-detach parameter type; no runtime action at this span. |
| [resume_bridge.rs:163:30](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L163) | `CR::sm_stream_owner` | I | read best-effort SM reverse index to find a candidate JID. |
| [resume_bridge.rs:166:32](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L166) | `CR::entry` | I | read and clone the candidate's current route entry. |
| [resume_bridge.rs:172:15](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L172) | `CE::is_locally_hosted` | A/I | read hosting to reject a nonlocal candidate. |
| [resume_bridge.rs:172:44](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L172) | `CE::sm_stream_id` | A/I | read entry SM ID and reject a stale reverse-index hit. |
| [resume_bridge.rs:191:31](../../../server/crates/waddle-server/src/clustering/resume_bridge.rs#L191) | `CE::try_force_detach_and_interrupt` | A/T | offer force detach and interrupt the captured local binding. |
| [owner.rs:3:83](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L3) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [owner.rs:227:65](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L227) | `CR::try_lock_bind` | A/T | try to acquire the JID bind gate before remote registration. |
| [owner.rs:280:22](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L280) | `CR::is_owned_by` | A/I | test whether the incumbent token still owns the route slot. |
| [owner.rs:280:61](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L280) | `CE::binding_id` | A | read the incumbent entry's local binding token. |
| [owner.rs:373:21](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L373) | `CEF::TYPE` | — | type-qualified constructor reference; no separate runtime action. |
| [owner.rs:373:38](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L373) | `CE::remote_hosted_for_connection` | O/I/T | construct a remote-hosted owner mirror with the source connection reference. |
| [owner.rs:378:27](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L378) | `CE::binding_id` | A/O | read the newly created mirror's local owner token. |
| [owner.rs:379:37](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L379) | `CE::take_force_detach_rx` | T/O | take the mirror's force-detach receiver for its forwarder. |
| [owner.rs:454:22](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L454) | `CR::register_entry_if_owner_or_absent` | A/I/O | publish the mirror only if the slot is absent or matches its owner. |
| [owner.rs:483:30](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L483) | `CR::unregister_if_owner` | A/I/O | remove only this mirror owner after a failed current-registration check. |
| [owner.rs:494:26](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L494) | `CR::unregister_if_owner` | A/I/O | remove only this mirror owner after a failed generation check. |
| [owner.rs:608:14](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L608) | `CR::unregister_if_owner` | A/I/O | remove the registered mirror only if its token remains current. |
| [side_effects.rs:2:48](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L2) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [side_effects.rs:2:65](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L2) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [side_effects.rs:420:16](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L420) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [side_effects.rs:427:18](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L427) | `CR::update_presence_if_owner` | A/I/O | owner-gated write of mirrored availability and priority. |
| [side_effects.rs:437:18](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L437) | `CR::update_presence_state_if_owner` | A/I/O | owner-gated write of the mirrored full presence snapshot. |
| [side_effects.rs:446:18](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L446) | `CR::clear_presence_state_if_owner` | A/I/O | owner-gated removal of the mirrored full presence snapshot. |
| [side_effects.rs:451:13](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L451) | `CEF::TYPE` | — | parameter type; no runtime action at this span. |
| [side_effects.rs:454:33](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L454) | `CEF.occupancy_session` | A/I/O | lock and write the mirror's occupancy generation from the remote snapshot. |
| [side_effects.rs:457:5](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L457) | `CEF.carbons_enabled` | A/I/O | store the remote snapshot's carbons flag in the mirror. |
| [side_effects.rs:460:5](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L460) | `CEF.roster_interested` | A/I/O | store the remote snapshot's roster-interest flag in the mirror. |
| [side_effects.rs:463:5](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L463) | `CEF.blocklist_interested` | A/I/O | store the remote snapshot's blocklist-interest flag in the mirror. |
| [side_effects.rs:466:5](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L466) | `CEF.presence_available` | A/I/O | store the remote snapshot's availability flag in the mirror. |
| [side_effects.rs:469:5](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/side_effects.rs#L469) | `CEF.presence_priority` | A/I/O | store the remote snapshot's priority in the mirror. |
| [flush.rs:2:65](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L2) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [flush.rs:146:16](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L146) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [flush.rs:169:16](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L169) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [flush.rs:191:40](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L191) | `CR::entry_if_owner` | A/I | read only the still-owned entry before retrying a flush. |
| [flush.rs:194:23](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L194) | `CE::is_locally_hosted` | A/I | reject a nonlocal entry before recovery work. |
| [flush.rs:197:23](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L197) | `CE::is_presence_available` | A/I | read availability to decide whether recovery may continue. |
| [flush.rs:197:56](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L197) | `CE::presence_priority` | A/I | read priority to decide whether recovery may continue. |
| [flush.rs:198:23](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L198) | `CE::reset_offline_flush` | A | reset the entry's offline-flush CAS after ineligibility. |
| [flush.rs:245:16](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L245) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [flush.rs:278:16](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L278) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [flush.rs:619:30](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L619) | `CR::try_reserve_pending_flush` | A/T | try to reserve capacity for the captured pending-delivery owner. |
| [flush.rs:621:30](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L621) | `CR::reserve_pending_flush` | A/T | await capacity reservation for the captured pending-delivery owner. |
| [flush.rs:701:30](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L701) | `CR::send_pending_flush_if_owner` | A/T | enqueue a row-tagged frame only for the supplied owner. |
| [flush.rs:712:30](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L712) | `CR::send_pending_flush` | T | enqueue a row-tagged frame through the unqualified path. |
| [flush.rs:728:45](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L728) | `CR::send_to_if_owner` | A/T | send a non-SM frame through the supplied-owner path. |
| [flush.rs:729:38](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L729) | `CR::send_to` | T | send a non-SM frame through the unqualified path. |
| [flush.rs:786:16](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L786) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [flush.rs:804:36](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L804) | `CR::entry_if_owner` | A/I | read only the still-owned entry for a recovery pass. |
| [flush.rs:807:19](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L807) | `CE::is_locally_hosted` | A/I | reject a nonlocal recovery target. |
| [flush.rs:808:23](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L808) | `CE::is_presence_available` | A/I | read availability for recovery eligibility. |
| [flush.rs:809:22](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L809) | `CE::presence_priority` | A/I | read priority for recovery eligibility. |
| [flush.rs:832:16](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L832) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [flush.rs:852:20](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L852) | `CR::send_reserved_pending_flush` | A/T | recheck and commit a previously reserved pending frame. |
| [flush.rs:880:16](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L880) | `CRF::TYPE` | — | parameter type; no runtime action at this span. |
| [implementation.rs:1432:10](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1432) | `CR::try_send_outbound_to_locally_hosted` | A/T | try nonblocking enqueue only to a locally hosted entry. |
| [implementation.rs:1468:10](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1468) | `CR::entry` | I | capture the current route entry before readiness awaits. |
| [implementation.rs:1469:31](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1469) | `CE::is_locally_hosted` | A/I | filter the captured entry to a locally hosted socket. |
| [implementation.rs:1489:24](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1489) | `CE::sm_stream_id` | A/I | read the captured entry's stream ID for dispatch checks. |
| [implementation.rs:1530:51](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1530) | `CR::capture_owned_enqueue_if_owner` | A/T | capture an enqueue bound to the selected local owner. |
| [implementation.rs:1532:15](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1532) | `CE::binding_id` | A | read the captured entry's binding token for that enqueue. |
| [registration.rs:53:30](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L53) | `CR::lock_bind` | A/T | acquire the per-JID bind gate for socket admission. |
| [registration.rs:90:49](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L90) | `CE::binding_ref` | A/T | read incumbent binding identity for a detach request. |
| [registration.rs:95:33](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L95) | `CE::try_force_detach_and_revoke` | A/T | offer detach and revoke the incumbent's pre-write permit. |
| [registration.rs:99:22](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L99) | `CE::try_force_detach_and_interrupt` | A/T | offer detach and interrupt the incumbent's binding. |
| [registration.rs:249:10](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L249) | `CR::unregister_if_owner` | A/I/T | remove only the captured owner during rollback. |
| [registration.rs:374:10](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L374) | `CR::set_sm_stream_id_if_owner` | A/I | set the SM stream ID and its index only for the captured owner. |
| [registration.rs:407:18](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L407) | `CR::update_presence_if_owner` | A/I | set restored availability and priority only for the captured owner. |
| [registration.rs:408:18](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L408) | `CR::update_presence_state_if_owner` | A/I | set restored full presence only for the captured owner. |
| [registration.rs:642:49](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L642) | `CEF::TYPE` | — | type-qualified constructor reference; no separate runtime action. |
| [registration.rs:642:66](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L642) | `CE::new_for_binding_with_local_io` | A/T | construct a local entry with the physical socket's I/O witness. |
| [registration.rs:647:40](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L647) | `CEF::TYPE` | — | type-qualified constructor reference; no separate runtime action. |
| [registration.rs:647:57](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L647) | `CE::new_for_binding` | A/T | construct a local entry for the exact binding without a witness. |
| [registration.rs:649:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L649) | `CEF.write_interruption` | T | replace the new entry's interruption handle with the socket owner's shared handle. |
| [registration.rs:650:15](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L650) | `CE::wire_resume_control` | A/T | wire exact owner-serviced resume control before publication. |
| [registration.rs:654:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L654) | `CEF.carbons_enabled` | A/I | seed the entry's carbons flag from socket state. |
| [registration.rs:657:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L657) | `CEF.roster_interested` | A/I | seed the entry's roster-interest flag from socket state. |
| [registration.rs:660:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L660) | `CEF.blocklist_interested` | A/I | seed the entry's blocklist-interest flag from socket state. |
| [registration.rs:668:10](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L668) | `CR::register_socket_entry` | A/I/T | publish the socket entry under its physical write-stop gate. |
| [registration.rs:678:10](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L678) | `CR::entry_if_owner` | A/I | read the published entry only if the captured owner remains current. |
| [registration.rs:688:10](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L688) | `CR::set_occupancy_session_if_owner` | A/I | publish occupancy generation only for the captured owner. |
| [registration.rs:731:14](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L731) | `CR::entry_if_owner` | A/I | read the current owned entry before restoring subscription-flush state. |
| [registration.rs:733:27](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L733) | `CE::claim_pending_subscribes_flush` | A | claim the entry's pending-subscription flush flag. |
| [registration.rs:758:10](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L758) | `CR::entry_if_owner` | A/I | recapture the current owned entry for actor/remote registration. |
| [registration.rs:801:18](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L801) | `CR::unregister_if_owner` | A/I/T | remove only the captured owner when actor/remote registration fails. |
| [registration.rs:885:35](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L885) | `CEF::TYPE` | — | clustering function parameter type; no runtime action at this span. |
| [registration.rs:943:36](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L943) | `CEF::TYPE` | — | non-clustering function parameter type; no runtime action at this span. |

| [socket.rs:4:20](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L4) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [socket.rs:214:16](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L214) | `CEF::TYPE` | — | registration parameter type; no runtime action at this span. |
| [socket.rs:242:22](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L242) | `CE::connection_ref` | A/T | read the socket's physical connection lease for the active-node check. |
| [socket.rs:259:42](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L259) | `CR::presence_state` | A/I | read the current full presence snapshot to build a remote registration. |
| [socket.rs:328:34](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L328) | `CE::connection_ref` | A/T | reread the source physical connection lease before registration commit. |
| [socket.rs:334:26](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L334) | `CR::entry_if_owner` | A/I | fetch the socket entry only if the captured owner remains current. |
| [socket.rs:335:56](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L335) | `CE::connection_ref` | A/T | read the current entry's physical connection identity for equality. |
| [socket.rs:335:82](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L335) | `CE::connection_ref` | A/T | read the captured entry's physical connection identity for equality. |
| [socket.rs:345:55](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L345) | `CE::connection_ref` | A/I/O | copy the source physical identity into the socket registration record. |
| [socket.rs:821:14](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L821) | `CR::entry_if_owner` | A/I | require the registered owner to remain current when looking up the socket. |
| [socket.rs:822:35](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L822) | `CE::connection_ref` | A/T | compare the current physical connection with the registration. |
| [socket.rs:931:56](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L931) | `CR::entry_if_owner` | A/I | read the current owner entry before resynchronization. |
| [socket.rs:958:26](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L958) | `CR::entry_if_owner` | A/I | recheck owner after remote re-registration before accepting it. |
| [socket.rs:987:68](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L987) | `CR::entry_if_owner` | A/I | recapture the owner entry before promoting to the local actor mirror. |
| [socket.rs:1064:42](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1064) | `CR::presence_state` | A/I/O | read the presence snapshot for remote full-state resynchronization. |
| [socket.rs:1127:42](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1127) | `CR::presence_state` | A/I/O | reread presence to check whether the resynchronization converged. |
| [socket.rs:1157:17](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1157) | `CEF::TYPE` | — | resynchronization parameter type; no runtime action at this span. |
| [socket.rs:1163:28](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1163) | `CEF.presence_available` | A/I/O | read availability for the remote full-state update. |
| [socket.rs:1164:27](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1164) | `CEF.presence_priority` | A/I/O | read priority for the remote full-state update. |
| [socket.rs:1168:26](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1168) | `CEF.carbons_enabled` | A/I/O | read carbons state for the remote full-state update. |
| [socket.rs:1171:12](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1171) | `CEF.roster_interested` | A/I/O | read roster interest to decide whether to include an update. |
| [socket.rs:1174:12](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1174) | `CEF.blocklist_interested` | A/I/O | read blocklist interest to decide whether to include an update. |
| [socket.rs:1250:14](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1250) | `CR::entry_if_owner` | A/I | retrieve only the socket belonging to the registered owner. |
| [socket.rs:1262:26](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1262) | `CE::occupancy_session` | A/I | compare the current entry's occupancy generation before detach. |
| [socket.rs:1272:35](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1272) | `CE::binding_ref` | A/T | copy the local binding identity into the detach request. |
| [socket.rs:1278:19](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1278) | `CE::try_force_detach_and_interrupt` | A/T | offer detach and interrupt the captured binding for the applicable origin. |
| [socket.rs:1280:19](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1280) | `CE::try_force_detach_for_jid` | A/T | offer an owner-managed detach to the captured JID and binding. |
| [socket.rs:1334:50](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1334) | `CR::entry` | I | read the current entry while assessing fresh remote retirement. |
| [socket.rs:1335:36](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1335) | `CEF::TYPE` | — | type-qualified method reference; no separate runtime action. |
| [socket.rs:1335:53](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1335) | `CE::occupancy_session` | A/I | read the entry's occupancy generation to check retirement identity. |
| [socket.rs:1341:14](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1341) | `CR::probe_socket_lifecycle` | A/T | probe socket lifecycle for the specified generation. |
| [socket.rs:1349:27](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1349) | `CE::is_locally_hosted` | A/I | check whether the remaining entry is hosted on this node. |
| [socket.rs:1351:30](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1351) | `CE::occupancy_session` | A/I | read its generation when deciding whether retirement remains pending. |
| [recovery.rs:11:25](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L11) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [recovery.rs:30:37](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L30) | `CRF::TYPE` | — | helper parameter type; no runtime action at this span. |
| [recovery.rs:32:14](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L32) | `CR::entry_if_owner` | A/I | fetch the current entry only for the recorded owner. |
| [recovery.rs:34:23](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L34) | `CE::is_locally_hosted` | A/I | check local hosting before treating the recovery target as current. |
| [recovery.rs:35:30](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L35) | `CE::is_presence_available` | A/I | read availability for current-target eligibility. |
| [recovery.rs:36:30](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L36) | `CE::presence_priority` | A/I | read priority for current-target eligibility. |
| [recovery.rs:171:34](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L171) | `CR::list_connections` | I | enumerate route JIDs for a pending-delivery recovery tick. |
| [recovery.rs:172:40](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L172) | `CR::entry` | I | fetch the current entry for each enumerated route. |
| [recovery.rs:175:23](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L175) | `CE::is_locally_hosted` | A/I | skip entries that are not locally hosted. |
| [recovery.rs:176:27](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L176) | `CE::is_presence_available` | A/I | skip unavailable resources. |
| [recovery.rs:177:26](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L177) | `CE::presence_priority` | A/I | skip negative-priority resources. |
| [recovery.rs:183:30](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L183) | `CE::binding_id` | A | capture the current local binding token for the recovery target. |
| [recovery.rs:184:32](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L184) | `CE::sm_stream_id` | A/I | capture the entry's SM stream ID for the recovery target. |
| [recovery.rs:185:33](../../../server/crates/waddle-server/src/pending_delivery/recovery.rs#L185) | `CE::presence_priority` | A/I | capture priority used to order or select recovery targets. |
| [regular.rs:70:18](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L70) | `CR::update_presence_if_owner` | A/I | update availability and priority only for the current owner. |
| [regular.rs:79:18](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L79) | `CR::clear_last_activity` | A | clear the bare-JID offline-activity record on available presence. |
| [regular.rs:97:18](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L97) | `CR::update_presence_state_if_owner` | A/I | write the full presence snapshot only for the current owner. |
| [regular.rs:133:18](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L133) | `CR::entry_if_owner` | A/I | fetch the still-owned entry before a subscription flush claim. |
| [regular.rs:134:44](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L134) | `CE::claim_pending_subscribes_flush` | A | atomically claim the once-per-entry subscription flush. |
| [regular.rs:140:22](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L140) | `CR::pending_subscription_stanzas` | A | read queued subscription stanzas without draining them. |
| [regular.rs:166:14](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L166) | `CR::clear_presence_state_if_owner` | A/I | remove the full presence snapshot only for the current owner. |
| [regular.rs:172:14](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L172) | `CR::record_last_activity` | A | write a bare-JID offline-activity timestamp and optional status. |
| [regular.rs:282:10](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L282) | `CR::entry_if_owner` | A/I | fetch only the still-owned entry before planning offline delivery. |
| [regular.rs:283:15](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L283) | `CE::claim_offline_flush` | A | atomically claim the once-per-entry offline flush. |
| [regular.rs:287:27](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L287) | `CE::sm_stream_id` | A/I | capture the entry SM stream ID for the flush plan. |
| [regular.rs:344:34](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L344) | `CR::send_to_if_owner` | A/T | send a queued subscription frame through the captured-owner path. |
| [regular.rs:346:40](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L346) | `CR::entry_if_owner` | A/I | recheck whether the captured owner is still registered. |
| [regular.rs:389:51](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L389) | `CR::entry_if_owner` | A/I | re-fetch the current owner before resetting a deferred flush. |
| [regular.rs:390:31](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L390) | `CE::reset_offline_flush` | A | reset the offline-flush CAS after transient deferred work. |
| [regular.rs:458:14](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L458) | `CR::available_resources_for_user` | A/I | enumerate subscriber resources with available presence. |
| [regular.rs:466:18](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L466) | `CR::send_to` | T | enqueue a broadcast presence through the unqualified route path. |
| [regular.rs:547:10](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L547) | `CR::send_to` | T | enqueue a capability-discovery IQ to the requesting resource. |
| [roster_fanout.rs:12:47](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L12) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [roster_fanout.rs:104:12](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L104) | `CEF::TYPE` | — | helper parameter type; no runtime action at this span. |
| [roster_fanout.rs:106:9](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L106) | `CEF.roster_interested` | A/I | read the entry's roster-interest flag before queueing. |
| [roster_fanout.rs:114:10](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L114) | `CR::try_send_outbound_if_owner` | A/T | try an owner-checked roster-push enqueue. |
| [roster_fanout.rs:114:54](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L114) | `CE::binding_id` | A | read the entry's local binding token for that enqueue. |
| [roster_fanout.rs:125:26](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L125) | `CR::entry` | I | fetch the current registry entry for a candidate resource. |
| [roster_fanout.rs:126:9](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L126) | `CEF.roster_interested` | A/I | read roster interest before local or remote fanout. |
| [roster_fanout.rs:131:14](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L131) | `CE::is_locally_hosted` | A/I | check whether the selected entry is locally hosted. |
| [roster_fanout.rs:165:53](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L165) | `CR::entry` | I | recapture the current entry before attempting local queueing. |
| [roster_fanout.rs:241:25](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L241) | `CR::roster_interested_resources_for_user` | A/I | enumerate resources currently marked roster-interested. |
| [roster_fanout.rs:253:14](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L253) | `CR::entry` | I | fetch each enumerated entry before checking hosting. |
| [roster_fanout.rs:254:35](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L254) | `CE::is_locally_hosted` | A/I | filter the candidate to locally hosted entries. |
| [roster_fanout.rs:311:10](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L311) | `CR::roster_interested_resources_for_user` | A/I | enumerate current roster-interested resources for the fallback fanout. |
| [archive_dispatch.rs:9:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L9) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [archive_dispatch.rs:9:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L9) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [archive_dispatch.rs:74:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L74) | `CEF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [archive_dispatch.rs:82:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L82) | `CE::try_send_archive_ordered_with_refusal` | A/T | invoke the refusal-preserving archive-aware enqueue and map the error. |
| [archive_dispatch.rs:102:36](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L102) | `CEF.endpoint` | A/T | enter the endpoint admission gate while deciding carbon suppression. |
| [archive_dispatch.rs:102:72](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L102) | `CE::is_carbons_enabled` | A/I | read carbons opt-in under the admission gate. |
| [archive_dispatch.rs:117:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L117) | `CEF.endpoint` | T | try direct endpoint enqueue when no archive positions require a frontier. |
| [archive_dispatch.rs:128:40](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L128) | `CE::sm_stream_id` | A/I | read SM stream ID and reject an archive dispatch for a different stream. |
| [archive_dispatch.rs:138:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L138) | `CEF.endpoint` | A/T | enter active endpoint admission with the original outbound envelope. |
| [archive_dispatch.rs:142:37](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L142) | `CEF.archive_dispatch` | A/T | lock and read or advance the archive frontier around enqueue. |
| [archive_dispatch.rs:193:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L193) | `CRF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [archive_dispatch.rs:195:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L195) | `CRF.connections` | I | read the route map for a requested local SM stream. |
| [archive_dispatch.rs:197:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L197) | `CE::is_locally_hosted` | A/I | filter the candidate to a locally hosted entry. |
| [archive_dispatch.rs:198:37](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L198) | `CE::sm_stream_id` | A/I | read the candidate entry's SM stream ID. |
| [implementation.rs:10:13](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L10) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [implementation.rs:31:71](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L31) | `CEF::TYPE` | — | resource-map value type; no runtime action at this span. |
| [implementation.rs:119:19](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L119) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint of a removed resource. |
| [implementation.rs:151:49](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L151) | `CE::binding_id` | A | read the entry owner token before conditional actor removal. |
| [implementation.rs:191:16](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L191) | `CEF::TYPE` | — | registration-message field type; no runtime action at this span. |
| [implementation.rs:213:19](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L213) | `CE::seal_actor_binding_if_any` | A/T | seal a replaced actor-bound entry before it is retired. |
| [implementation.rs:239:16](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L239) | `CEF::TYPE` | — | conditional-registration-message field type; no runtime action at this span. |
| [implementation.rs:251:22](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L251) | `CE::binding_id` | A | read the proposed entry's token to validate the registration request. |
| [implementation.rs:274:40](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L274) | `CE::binding_id` | A | read an incumbent token to reject an ownership mismatch. |
| [implementation.rs:297:19](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L297) | `CE::seal_actor_binding_if_any` | A/T | seal the incumbent actor-bound endpoint before replacement. |
| [implementation.rs:552:34](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L552) | `CE::is_presence_available` | A/I | read resource availability for a returned snapshot. |
| [implementation.rs:553:33](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L553) | `CE::presence_priority` | A/I | read resource priority for a returned snapshot. |
| [implementation.rs:554:34](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L554) | `CEF.carbons_enabled` | A/I | load carbons opt-in for a returned snapshot. |
| [implementation.rs:592:40](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L592) | `CE::is_presence_available` | A/I | read availability when selecting available resources. |
| [implementation.rs:593:53](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L593) | `CE::presence_priority` | A/I | read priority for each selected available resource. |
| [implementation.rs:635:13](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L635) | `CEF.presence_available` | A/I | store the actor update's availability in the shared entry. |
| [implementation.rs:638:13](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L638) | `CEF.presence_priority` | A/I | store the actor update's priority in the shared entry. |
| [implementation.rs:779:13](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L779) | `CEF.carbons_enabled` | A/I | store the actor update's carbons flag in the shared entry. |
| [implementation.rs:799:32](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L799) | `CE::is_carbons_enabled` | A/I | read carbons opt-in for the actor's query response. |

| [presence.rs:2:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L2) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [presence.rs:7:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L7) | `CRF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [presence.rs:12:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L12) | `CRF.connections` | I | look up the current route entry for an unqualified presence update. |
| [presence.rs:13:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L13) | `CEF.presence_available` | A/I | store availability into the matched entry without an owner check. |
| [presence.rs:17:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L17) | `CEF.presence_priority` | A/I | store priority into the matched entry without an owner check. |
| [presence.rs:40:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L40) | `CRF.connections` | I | fetch and guard the current route entry for an owner-qualified update. |
| [presence.rs:43:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L43) | `CE::binding_id` | A | compare the entry binding token with the supplied owner. |
| [presence.rs:46:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L46) | `CEF.presence_available` | A/I | store availability after the owner check. |
| [presence.rs:50:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L50) | `CEF.presence_priority` | A/I | store priority after the owner check. |
| [presence.rs:81:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L81) | `CRF.connections` | I | fetch and guard the current route entry before writing full presence. |
| [presence.rs:84:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L84) | `CE::binding_id` | A | compare the entry binding token with the supplied owner. |
| [presence.rs:87:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L87) | `CRF.presence_states` | A/I | insert a full presence snapshot while the route guard remains held. |
| [presence.rs:110:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L110) | `CRF.presence_states` | A/I | insert a full presence snapshot without an owner check. |
| [presence.rs:123:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L123) | `CRF.presence_states` | A/I | read and clone the full presence snapshot for the JID. |
| [presence.rs:128:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L128) | `CRF.presence_states` | A/I | remove the full presence snapshot without an owner check. |
| [presence.rs:144:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L144) | `CRF.connections` | I | fetch and guard the current route entry before clearing presence. |
| [presence.rs:147:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L147) | `CE::binding_id` | A | compare the entry binding token with the supplied owner. |
| [presence.rs:150:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L150) | `CRF.presence_states` | A/I | remove the full presence snapshot while the route guard remains held. |
| [presence.rs:156:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L156) | `CRF.last_activity` | A | insert last offline activity for the bare JID. |
| [presence.rs:167:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L167) | `CRF.last_activity` | A | look up the stored bare-JID offline activity. |
| [presence.rs:174:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L174) | `CRF.last_activity` | A | remove the stored bare-JID offline activity. |
| [presence.rs:179:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L179) | `CRF.started_at` | A | read the process start instant to calculate uptime. |
| [presence.rs:184:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L184) | `CRF.connections` | I | iterate route entries for the requested bare JID. |
| [presence.rs:187:69](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L187) | `CE::is_presence_available` | A/I | filter the entries by current available-presence state. |
| [presence.rs:189:62](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L189) | `CE::presence_priority` | A/I | read priority for each available resource. |
| [presence.rs:199:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L199) | `CRF.connections` | I | scan the route map to collect JIDs with closed endpoints. |
| [presence.rs:202:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L202) | `CEF.endpoint` | T | inspect each entry endpoint's closed state. |
| [presence.rs:207:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L207) | `CR::unregister` | I/T | remove the scanned JID through unqualified unregister. |
| [resources.rs:2:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L2) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [resources.rs:6:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L6) | `CRF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [resources.rs:29:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L29) | `CRF.connections` | I | fetch and guard the current entry for roster-interest mutation. |
| [resources.rs:32:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L32) | `CE::binding_id` | A | compare the entry token with the supplied owner. |
| [resources.rs:35:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L35) | `CEF.roster_interested` | A/I | store roster interest after the owner check. |
| [resources.rs:41:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L41) | `CRF.connections` | I | look up the entry for a roster-interest query. |
| [resources.rs:43:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L43) | `CEF.roster_interested` | A/I | read the entry's roster-interest flag. |
| [resources.rs:49:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L49) | `CRF.connections` | I | iterate route entries for roster-interested enumeration. |
| [resources.rs:53:24](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L53) | `CEF.roster_interested` | A/I | filter entries by their roster-interest flag. |
| [resources.rs:79:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L79) | `CRF.connections` | I | fetch and guard the current entry for blocklist-interest mutation. |
| [resources.rs:82:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L82) | `CE::binding_id` | A | compare the entry token with the supplied owner. |
| [resources.rs:85:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L85) | `CEF.blocklist_interested` | A/I | store blocklist interest after the owner check. |
| [resources.rs:91:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L91) | `CRF.connections` | I | look up the entry for a blocklist-interest query. |
| [resources.rs:93:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L93) | `CEF.blocklist_interested` | A/I | read the entry's blocklist-interest flag. |
| [resources.rs:99:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L99) | `CRF.connections` | I | iterate route entries for blocklist-interested enumeration. |
| [resources.rs:103:24](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L103) | `CEF.blocklist_interested` | A/I | filter entries by their blocklist-interest flag. |
| [resources.rs:123:31](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L123) | `CR::send_to` | T | invoke unqualified async send for each fanout recipient. |
| [resources.rs:134:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L134) | `CRF.connections` | I | read a snapshot of all registered route JIDs. |
| [resources.rs:146:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L146) | `CRF.connections` | I | iterate route entries to collect active SM IDs. |
| [resources.rs:148:47](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L148) | `CE::sm_stream_id` | A/I | read each entry's published SM ID for the active set. |
| [resources.rs:163:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L163) | `CRF.connections` | I | enumerate other full-JID resources for a bare JID. |
| [resources.rs:187:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L187) | `CRF.connections` | I | enumerate candidate carbon resources for a bare JID. |
| [resources.rs:193:38](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L193) | `CE::is_carbons_enabled` | A/I | filter carbon candidates by per-entry opt-in. |
| [resources.rs:203:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L203) | `CRF.connections` | I | fetch the route entry for a carbons query. |
| [resources.rs:205:40](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L205) | `CE::is_carbons_enabled` | A/I | read that entry's carbons opt-in. |
| [resources.rs:232:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L232) | `CRF.connections` | I | fetch and guard the current entry for carbons mutation. |
| [resources.rs:235:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L235) | `CE::binding_id` | A | compare the entry token with the supplied owner. |
| [resources.rs:238:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L238) | `CEF.carbons_enabled` | A/I | store carbons opt-in after the owner check. |
| [subscriptions.rs:1:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/subscriptions.rs#L1) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [subscriptions.rs:3:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/subscriptions.rs#L3) | `CRF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [subscriptions.rs:8:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/subscriptions.rs#L8) | `CRF.pending_subscription_stanzas` | A | obtain the bare-JID queue, deduplicate subscribe requests, and append the new stanza. |
| [subscriptions.rs:34:31](../../../server/crates/waddle-xmpp/src/registry/connection_registry/subscriptions.rs#L34) | `CRF.pending_subscription_stanzas` | A | obtain and mutate the queue to remove matching subscribe requests. |
| [subscriptions.rs:54:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/subscriptions.rs#L54) | `CRF.pending_subscription_stanzas` | A | remove the bare-JID queue and return its stanzas. |
| [subscriptions.rs:67:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/subscriptions.rs#L67) | `CRF.pending_subscription_stanzas` | A | read and clone the bare-JID queue without draining it. |

| [connections.rs:2:78](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L2) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [connections.rs:3:5](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L3) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [connections.rs:10:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L10) | `CRF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [connections.rs:93:23](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L93) | `CRF.connections` | A/I | conditionally remove the exact provisional candidate from the route map. |
| [connections.rs:97:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L97) | `CEF.binding_permit` | A/T | revoke the candidate's shared permit with notification deferred. |
| [connections.rs:98:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L98) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint belonging to the removed candidate. |
| [connections.rs:99:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L99) | `CR::remove_sm_stream_owner` | I | prune the candidate's SM reverse-index entry under the route guard. |
| [connections.rs:99:43](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L99) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [connections.rs:100:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L100) | `CR::remove_retired_presence` | I | prune the removed candidate's full-presence snapshot under the route guard. |
| [connections.rs:100:44](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L100) | `CRF.presence_states` | I | pass the presence map to the guarded pruning helper. |
| [connections.rs:104:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L104) | `CEF.binding_permit` | T | notify waiters that the removed candidate's permit was revoked. |
| [connections.rs:123:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L123) | `CR::register_with_carbons` | I/T | delegate registration with the default disabled carbons flag. |
| [connections.rs:137:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L137) | `CR::register_with_stream_state` | I/T | delegate registration with the supplied carbons and default interest flags. |
| [connections.rs:150:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L150) | `CEF::TYPE` | — | type-qualified constructor reference; no separate runtime action. |
| [connections.rs:150:38](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L150) | `CE::new` | A/T | construct a fresh local entry for the endpoint. |
| [connections.rs:152:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L152) | `CEF.carbons_enabled` | A/I | seed carbons opt-in on the newly constructed entry. |
| [connections.rs:155:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L155) | `CEF.roster_interested` | A/I | seed roster interest on the newly constructed entry. |
| [connections.rs:158:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L158) | `CEF.blocklist_interested` | A/I | seed blocklist interest on the newly constructed entry. |
| [connections.rs:160:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L160) | `CR::register_entry` | I/T | publish the constructed entry in the route map. |
| [connections.rs:171:55](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L171) | `CEF::TYPE` | — | entry parameter type; no runtime action at this span. |
| [connections.rs:172:32](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L172) | `CE::binding_id` | A | capture the candidate entry's local binding token. |
| [connections.rs:173:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L173) | `CRF.connections` | A/I | access the route slot to insert or replace the entry. |
| [connections.rs:175:46](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L175) | `CE::same_exact_local_binding` | A/T | compare incumbent and candidate exact bindings before retirement. |
| [connections.rs:177:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L177) | `CEF.binding_permit` | A/T | revoke a displaced incumbent's pre-write permit with notification deferred. |
| [connections.rs:178:36](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L178) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint on the displaced incumbent. |
| [connections.rs:180:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L180) | `CE::sm_stream_id` | A/I | read the incumbent's stream ID before deciding index cleanup. |
| [connections.rs:180:59](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L180) | `CE::sm_stream_id` | A/I | read the candidate's stream ID before deciding index cleanup. |
| [connections.rs:181:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L181) | `CR::remove_sm_stream_owner` | I | prune the old SM reverse mapping if the stream changed. |
| [connections.rs:181:51](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L181) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [connections.rs:195:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L195) | `CEF.binding_permit` | T | notify waiters after a displaced incumbent's permit was revoked. |
| [connections.rs:206:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L206) | `CEF::TYPE` | — | socket-entry parameter type; no runtime action at this span. |
| [connections.rs:208:32](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L208) | `CE::binding_id` | A | capture the candidate entry's local binding token. |
| [connections.rs:209:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L209) | `CE::binding_ref` | A/T | obtain the candidate's exact local binding attempt. |
| [connections.rs:210:28](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L210) | `CEF.write_interruption` | T | clone its physical socket interruption handle for publication. |
| [connections.rs:211:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L211) | `CRF.connections` | A/I | access the route slot to publish or replace the socket entry. |
| [connections.rs:213:56](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L213) | `CE::sm_stream_id` | A/I | read the incumbent's SM ID to decide reverse-index cleanup. |
| [connections.rs:213:80](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L213) | `CE::sm_stream_id` | A/I | read the candidate's SM ID to decide reverse-index cleanup. |
| [connections.rs:214:51](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L214) | `CE::same_exact_local_binding` | A/T | compare incumbent and candidate exact binding identities. |
| [connections.rs:215:34](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L215) | `CEF.binding_permit` | A/T | clone the incumbent's permit state for ordered revocation. |
| [connections.rs:222:44](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L222) | `CE::seal_actor_binding_if_any` | A/T | seal an actor-bound incumbent when the exact binding changes. |
| [connections.rs:233:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L233) | `CR::remove_sm_stream_owner` | I | prune the old SM reverse mapping if the stream changed. |
| [connections.rs:233:51](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L233) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [connections.rs:248:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L248) | `CEF.binding_permit` | T | notify waiters after a displaced socket entry's permit was revoked. |
| [connections.rs:258:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L258) | `CEF::TYPE` | — | entry parameter type; no runtime action at this span. |
| [connections.rs:261:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L261) | `CE::binding_id` | A | compare the proposed entry token with the supplied owner. |
| [connections.rs:265:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L265) | `CRF.connections` | A/I | access the route slot for owner-or-absent publication. |
| [connections.rs:267:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L267) | `CE::binding_id` | A | reject an occupied slot owned by a different token. |
| [connections.rs:271:46](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L271) | `CE::same_exact_local_binding` | A/T | compare incumbent and candidate exact bindings before retirement. |
| [connections.rs:273:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L273) | `CEF.binding_permit` | A/T | defer revocation notification for the displaced incumbent. |
| [connections.rs:274:36](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L274) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint on the displaced incumbent. |
| [connections.rs:276:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L276) | `CE::sm_stream_id` | A/I | read the incumbent SM ID before reverse-index cleanup. |
| [connections.rs:276:59](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L276) | `CE::sm_stream_id` | A/I | read the candidate SM ID before reverse-index cleanup. |
| [connections.rs:277:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L277) | `CR::remove_sm_stream_owner` | I | prune the old SM reverse mapping if the stream changed. |
| [connections.rs:277:51](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L277) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [connections.rs:291:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L291) | `CEF.binding_permit` | T | notify waiters after a displaced owner's permit was revoked. |
| [connections.rs:300:55](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L300) | `CEF::TYPE` | — | return type; no runtime action at this span. |
| [connections.rs:301:23](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L301) | `CRF.connections` | I/T | remove the route entry unconditionally for this JID. |
| [connections.rs:302:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L302) | `CEF.binding_permit` | A/T | revoke the removed entry's permit with notification deferred. |
| [connections.rs:303:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L303) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint on the removed entry. |
| [connections.rs:304:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L304) | `CR::remove_sm_stream_owner` | I | prune the removed entry's SM reverse mapping under the route guard. |
| [connections.rs:304:43](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L304) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [connections.rs:305:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L305) | `CR::remove_retired_presence` | I | remove the retired entry's full-presence snapshot under the route guard. |
| [connections.rs:305:44](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L305) | `CRF.presence_states` | I | pass the presence map to the guarded pruning helper. |
| [connections.rs:309:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L309) | `CEF.binding_permit` | T | notify waiters after the removed entry's permit was revoked. |
| [connections.rs:323:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L323) | `CRF.connections` | I | access the current route entry to test ownership. |
| [connections.rs:325:40](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L325) | `CE::binding_id` | A | compare the current entry token with the supplied owner. |
| [connections.rs:335:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L335) | `CEF::TYPE` | — | return type; no runtime action at this span. |
| [connections.rs:336:23](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L336) | `CRF.connections` | A/I | remove the route entry only if its owner token matches. |
| [connections.rs:337:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L337) | `CE::binding_id` | A | compare the current entry token with the supplied owner. |
| [connections.rs:340:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L340) | `CEF.binding_permit` | A/T | revoke the matching owner's permit with notification deferred. |
| [connections.rs:341:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L341) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint on the matching entry. |
| [connections.rs:342:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L342) | `CR::remove_sm_stream_owner` | I | prune the matching owner's SM reverse mapping under the route guard. |
| [connections.rs:342:43](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L342) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [connections.rs:343:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L343) | `CR::remove_retired_presence` | I | remove the matching owner's full-presence snapshot under the route guard. |
| [connections.rs:343:44](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L343) | `CRF.presence_states` | I | pass the presence map to the guarded pruning helper. |
| [connections.rs:347:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L347) | `CEF.binding_permit` | T | notify waiters after the matching owner's permit was revoked. |
| [connections.rs:371:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L371) | `CEF::TYPE` | — | return type; no runtime action at this span. |
| [connections.rs:372:23](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L372) | `CRF.connections` | A/I | remove the route entry only if its SM session matches. |
| [connections.rs:373:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L373) | `CE::sm_stream_id` | A/I | compare the current entry's SM ID with the supplied session. |
| [connections.rs:376:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L376) | `CEF.binding_permit` | A/T | revoke the matching entry's permit with notification deferred. |
| [connections.rs:377:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L377) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint on the matching entry. |
| [connections.rs:378:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L378) | `CR::remove_sm_stream_owner` | I | prune the matching entry's SM reverse mapping under the route guard. |
| [connections.rs:378:43](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L378) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [connections.rs:379:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L379) | `CR::remove_retired_presence` | I | remove the matching entry's presence snapshot under the route guard. |
| [connections.rs:379:44](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L379) | `CRF.presence_states` | I | pass the presence map to the guarded pruning helper. |
| [connections.rs:383:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L383) | `CEF.binding_permit` | T | notify waiters after the matching entry's permit was revoked. |
| [connections.rs:397:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L397) | `CEF::TYPE` | — | return type; no runtime action at this span. |
| [connections.rs:398:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L398) | `CRF.connections` | I | fetch and clone an entry only if its owner matches. |
| [connections.rs:399:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L399) | `CE::binding_id` | A | compare the current entry token with the supplied owner. |
| [connections.rs:414:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L414) | `CRF.connections` | I | fetch and guard the current entry for local permit capture. |
| [connections.rs:415:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L415) | `CE::binding_id` | A | reject an entry with a different local binding token. |
| [connections.rs:416:23](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L416) | `CE::is_locally_hosted` | A/I | reject an entry that is not locally hosted. |
| [connections.rs:417:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L417) | `CEF.endpoint` | A/T | require the captured endpoint to be actively admitting. |
| [connections.rs:421:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L421) | `CE::binding_ref` | A/T | copy the exact local binding attempt into the permit. |
| [connections.rs:422:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L422) | `CEF.binding_permit` | A/T | require the shared binding permit to remain active. |
| [connections.rs:429:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L429) | `CEF.binding_permit` | A/T | clone the shared revocation cell into the returned permit. |
| [connections.rs:443:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L443) | `CR::entry_if_owner` | A/I | capture an entry only if its owner token remains current. |
| [connections.rs:444:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L444) | `CE::binding_ref` | A/T | reject a different local binding attempt. |
| [connections.rs:447:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L447) | `CE::resume_control` | T | obtain the bound socket's owner-serviced resume control. |
| [connections.rs:453:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L453) | `CR::local_binding_permit_if_owner` | A/T | capture the current owner's revocable local permit. |
| [connections.rs:454:65](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L454) | `CEF.binding_permit` | A/T | verify that the permit shares the captured entry's revocation state. |
| [connections.rs:466:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L466) | `CRF.connections` | I | check whether the route map currently contains the JID. |
| [connections.rs:472:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L472) | `CRF.connections` | I | fetch the current route entry to check local hosting. |
| [connections.rs:474:40](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L474) | `CE::is_locally_hosted` | A/I | read the current entry's local-hosting classification. |
| [connections.rs:482:50](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L482) | `CEF::TYPE` | — | return type; no runtime action at this span. |
| [connections.rs:483:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L483) | `CRF.connections` | I | fetch and clone the current route entry. |
| [connections.rs:488:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L488) | `CRF.connections` | I | read the number of entries in the route map. |

| [sending.rs:5:42](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L5) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [sending.rs:5:59](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L5) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [sending.rs:103:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L103) | `CEF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [sending.rs:115:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L115) | `CE::binding_id` | A | compare the current binding token against the pending reservation. |
| [sending.rs:116:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L116) | `CE::binding_ref` | A/T | compare the current binding attempt against the reservation. |
| [sending.rs:117:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L117) | `CEF.endpoint` | A/T | compare the current and reserved endpoint binding identities. |
| [sending.rs:118:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L118) | `CEF.endpoint` | A/T | verify that the endpoint still matches the reservation owner and binding. |
| [sending.rs:142:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L142) | `CRF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [sending.rs:150:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L150) | `CRF.connections` | I | fetch the current entry for a capacity reservation. |
| [sending.rs:153:60](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L153) | `CE::binding_id` | A | filter the entry by the optional supplied owner. |
| [sending.rs:155:24](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L155) | `CEF.endpoint` | T | clone the selected endpoint before waiting for capacity. |
| [sending.rs:156:36](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L156) | `CE::binding_id` | A | capture the selected entry's owner token. |
| [sending.rs:157:33](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L157) | `CE::binding_ref` | A/T | capture the selected entry's binding attempt. |
| [sending.rs:169:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L169) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint only if it is closed and still matches. |
| [sending.rs:184:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L184) | `CRF.connections` | I | fetch the current entry for a nonblocking reservation. |
| [sending.rs:187:60](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L187) | `CE::binding_id` | A | filter the entry by the optional supplied owner. |
| [sending.rs:189:24](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L189) | `CEF.endpoint` | T | clone the selected endpoint before reserving capacity. |
| [sending.rs:190:36](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L190) | `CE::binding_id` | A | capture the selected entry's owner token. |
| [sending.rs:191:33](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L191) | `CE::binding_ref` | A/T | capture the selected entry's binding attempt. |
| [sending.rs:203:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L203) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint only if it is closed and still matches. |
| [sending.rs:221:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L221) | `CRF.connections` | A/I | exclusively fetch the current route entry before reservation commit. |
| [sending.rs:222:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L222) | `CE::is_locally_hosted` | A/I | require the current entry to be locally hosted. |
| [sending.rs:223:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L223) | `CE::is_presence_available` | A/I | require the current entry to be available. |
| [sending.rs:224:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L224) | `CE::presence_priority` | A/I | require the current entry to have nonnegative priority. |
| [sending.rs:230:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L230) | `CE::commit_pending_flush` | A/T | commit the reserved envelope after exact-binding checks. |
| [sending.rs:237:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L237) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint if the commit proves it closed. |
| [sending.rs:255:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L255) | `CRF.connections` | I | look up an endpoint for a write-acceptance attempt. |
| [sending.rs:256:28](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L256) | `CEF.endpoint` | T | clone the endpoint captured for the async capacity wait. |
| [sending.rs:271:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L271) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint if reservation proves it closed. |
| [sending.rs:276:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L276) | `CRF.connections` | I | reacquire the current entry after waiting for capacity. |
| [sending.rs:282:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L282) | `CEF.endpoint` | A/T | require the post-wait entry to share the captured endpoint binding. |
| [sending.rs:296:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L296) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint if commit proves it closed. |
| [sending.rs:326:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L326) | `CRF.connections` | I | look up the entry for an ordinary direct send. |
| [sending.rs:327:28](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L327) | `CEF.endpoint` | T | clone its endpoint before awaiting delivery capacity. |
| [sending.rs:344:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L344) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint only if it remains closed and matches. |
| [sending.rs:345:38](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L345) | `CRF.connections` | I | look up the current route after the old endpoint fails. |
| [sending.rs:346:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L346) | `CEF.endpoint` | T | clone the current endpoint to detect and possibly retry a replacement. |
| [sending.rs:358:38](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L358) | `CR::remove_if_sender_closed_owner` | I/T | evict the replacement endpoint if its own send proves it closed. |
| [sending.rs:391:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L391) | `CRF.connections` | I | look up the current entry for an owner-qualified send. |
| [sending.rs:392:42](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L392) | `CE::binding_id` | A | compare the entry binding token with the supplied owner. |
| [sending.rs:392:68](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L392) | `CEF.endpoint` | T | clone the matching owner's endpoint for async send. |
| [sending.rs:404:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L404) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint only if it remains closed and matches. |
| [sending.rs:424:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L424) | `CRF.connections` | I | look up the entry for an unqualified pending-flush send. |
| [sending.rs:425:28](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L425) | `CEF.endpoint` | T | clone its endpoint for async row-tagged delivery. |
| [sending.rs:438:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L438) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint only if it remains closed and matches. |
| [sending.rs:469:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L469) | `CRF.connections` | I | look up the entry for an owner-qualified pending-flush send. |
| [sending.rs:470:42](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L470) | `CE::binding_id` | A | compare the entry binding token with the supplied owner. |
| [sending.rs:470:68](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L470) | `CEF.endpoint` | T | clone the matching owner's endpoint for async row-tagged delivery. |
| [sending.rs:483:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L483) | `CR::remove_if_sender_closed_owner` | I/T | evict the captured endpoint only if it remains closed and matches. |
| [sending.rs:513:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L513) | `CR::try_send_to_matching` | T | delegate a nonblocking send with an always-accepting match predicate. |
| [sending.rs:523:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L523) | `CRF.connections` | I | fetch an entry for a local-hosted nonblocking outbound send. |
| [sending.rs:526:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L526) | `CE::is_locally_hosted` | A/I | filter the entry to a locally hosted socket. |
| [sending.rs:531:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L531) | `CR::try_send_outbound_if_owner` | A/T | delegate outbound admission using the captured owner token. |
| [sending.rs:531:53](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L531) | `CE::binding_id` | A | read the captured local binding token for admission. |
| [sending.rs:538:31](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L538) | `CEF::TYPE` | — | match-predicate parameter type; no runtime action at this span. |
| [sending.rs:540:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L540) | `CRF.connections` | I | fetch the current route entry for nonblocking matched send. |
| [sending.rs:541:54](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L541) | `CEF.endpoint` | T | clone the endpoint if the supplied predicate accepts the entry. |
| [sending.rs:584:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L584) | `CR::remove_if_sender_closed` | I/T | remove the current route only if its endpoint is still closed. |
| [sending.rs:608:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L608) | `CRF.connections` | I | fetch the current route entry for owner-checked outbound admission. |
| [sending.rs:609:42](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L609) | `CE::binding_id` | A | require the current entry's binding token to match the supplied owner. |
| [sending.rs:624:21](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L624) | `CE::try_send_archive_ordered` | A/T | try archive-ordered endpoint admission with the original envelope. |
| [sending.rs:635:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L635) | `CR::remove_if_sender_closed_owner` | I/T | evict only a closed endpoint matching the captured entry. |
| [sending.rs:635:58](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L635) | `CEF.endpoint` | T | pass the captured endpoint identity to closed-owner cleanup. |
| [sending.rs:654:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L654) | `CRF.connections` | I | clone the route map handle into the captured enqueue closure. |
| [sending.rs:655:31](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L655) | `CRF.presence_states` | I | clone the presence map handle for closure-side closed-owner cleanup. |
| [sending.rs:656:32](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L656) | `CRF.sm_stream_owners` | I | clone the SM reverse-index handle for closure-side closed-owner cleanup. |
| [sending.rs:659:38](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L659) | `CE::binding_id` | A | compare the current entry token with the captured owner in the closure. |
| [sending.rs:665:25](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L665) | `CE::try_send_archive_ordered` | A/T | try archive-ordered endpoint admission within the captured closure. |
| [sending.rs:676:27](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L676) | `CR::remove_closed_owner_entry` | I/T | invoke exact-endpoint closed-entry cleanup from the closure. |
| [sending.rs:681:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L681) | `CEF.endpoint` | T | pass the captured entry's endpoint identity into the cleanup helper. |
| [sending.rs:702:23](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L702) | `CRF.connections` | I/T | remove the current JID entry only when its endpoint remains closed. |
| [sending.rs:703:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L703) | `CEF.endpoint` | T | inspect whether the current entry endpoint is closed. |
| [sending.rs:706:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L706) | `CEF.binding_permit` | A/T | revoke the closed entry's permit with notification deferred. |
| [sending.rs:707:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L707) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint on the closed entry. |
| [sending.rs:708:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L708) | `CR::remove_sm_stream_owner` | I | prune the closed entry's SM reverse mapping under the route guard. |
| [sending.rs:708:43](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L708) | `CRF.sm_stream_owners` | I | pass the SM reverse index to the guarded pruning helper. |
| [sending.rs:709:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L709) | `CR::remove_retired_presence` | I | remove the closed entry's presence snapshot under the route guard. |
| [sending.rs:709:44](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L709) | `CRF.presence_states` | I | pass the presence map to the guarded pruning helper. |
| [sending.rs:713:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L713) | `CEF.binding_permit` | T | notify waiters after the closed entry's permit was revoked. |
| [sending.rs:730:15](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L730) | `CR::remove_closed_owner_entry` | I/T | delegate closed-owner cleanup to the exact-endpoint helper. |
| [sending.rs:731:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L731) | `CRF.connections` | I | pass the route map to exact-endpoint cleanup. |
| [sending.rs:732:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L732) | `CRF.presence_states` | I | pass the presence map to exact-endpoint cleanup. |
| [sending.rs:733:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L733) | `CRF.sm_stream_owners` | I | pass the SM reverse index to exact-endpoint cleanup. |
| [sending.rs:740:40](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L740) | `CEF::TYPE` | — | map value type in the cleanup helper; no runtime action at this span. |
| [sending.rs:747:17](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L747) | `CEF.endpoint` | T | require the currently registered endpoint to be closed. |
| [sending.rs:747:48](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L747) | `CEF.endpoint` | A/T | require the currently registered endpoint to match the captured binding. |
| [sending.rs:750:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L750) | `CEF.binding_permit` | A/T | revoke the matching closed entry's permit with notification deferred. |
| [sending.rs:751:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L751) | `CE::seal_actor_binding_if_any` | A/T | seal any actor-bound endpoint on the matching closed entry. |
| [sending.rs:752:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L752) | `CR::remove_sm_stream_owner` | I | prune the matching closed entry's SM reverse mapping. |
| [sending.rs:753:19](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L753) | `CR::remove_retired_presence` | I | remove the matching closed entry's full-presence snapshot. |
| [sending.rs:757:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L757) | `CEF.binding_permit` | T | notify waiters after the matching closed entry's permit was revoked. |

| [outbound.rs:402:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L402) | `CEF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [outbound.rs:405:15](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L405) | `CE::new_for_connection` | A/T | delegate construction with a fresh physical connection reference. |
| [outbound.rs:417:15](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L417) | `CE::new_for_binding` | A/T | delegate construction with a fresh binding attempt for the physical connection. |
| [outbound.rs:426:15](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L426) | `CE::new_entry` | A/T | construct an active local entry for the supplied binding. |
| [outbound.rs:442:31](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L442) | `CE::new_for_binding` | A/T | construct an active entry before attaching a local I/O witness. |
| [outbound.rs:443:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L443) | `CEF.local_io_relinquishment` | T | attach the supplied physical socket's local I/O relinquishment witness. |
| [outbound.rs:457:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L457) | `CE::new_entry` | A/T | construct a provisional local entry that initially refuses admission. |
| [outbound.rs:482:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L482) | `CE::new_entry` | A/T | construct an actor-provisional entry after checking custody identity. |
| [outbound.rs:505:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L505) | `CEF.binding_id` | A | initialize the entry's freshly minted local binding token. |
| [outbound.rs:506:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L506) | `CEF.connection_ref` | A/T | initialize the physical connection identity. |
| [outbound.rs:507:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L507) | `CEF.binding_ref` | A/T | initialize the optional source binding attempt. |
| [outbound.rs:508:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L508) | `CEF.binding_permit` | A/T | initialize the shared binding permit state. |
| [outbound.rs:509:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L509) | `CEF.local_io_relinquishment` | T | initialize local I/O relinquishment to absent. |
| [outbound.rs:510:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L510) | `CEF.hosting` | A/I | initialize hosting as local before any remote-mirror override. |
| [outbound.rs:511:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L511) | `CEF.resume_control` | T | initialize the empty owner-serviced resume-control cell. |
| [outbound.rs:512:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L512) | `CEF.terminal_ordering_retry` | A/T | initialize the terminal-ordering retry flag as unclaimed. |
| [outbound.rs:513:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L513) | `CEF.archive_dispatch` | A/T | initialize empty archive-dispatch frontiers. |
| [outbound.rs:514:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L514) | `CEF.endpoint` | T | initialize the entry's binding-scoped outbound endpoint. |
| [outbound.rs:515:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L515) | `CEF.carbons_enabled` | A/I | initialize carbons opt-in as disabled. |
| [outbound.rs:516:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L516) | `CEF.presence_available` | A/I | initialize presence availability as false. |
| [outbound.rs:517:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L517) | `CEF.presence_priority` | A/I | initialize presence priority to zero. |
| [outbound.rs:518:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L518) | `CEF.roster_interested` | A/I | initialize roster interest as false. |
| [outbound.rs:519:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L519) | `CEF.blocklist_interested` | A/I | initialize blocklist interest as false. |
| [outbound.rs:520:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L520) | `CEF.offline_flushed` | A | initialize the offline-flush claim as false. |
| [outbound.rs:521:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L521) | `CEF.pending_subscribes_flushed` | A | initialize the pending-subscription flush claim as false. |
| [outbound.rs:522:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L522) | `CEF.sm_stream_id` | A/I | initialize the SM stream ID as absent. |
| [outbound.rs:523:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L523) | `CEF.occupancy_session` | A/I | initialize occupancy generation as absent. |
| [outbound.rs:524:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L524) | `CEF.force_detach_tx` | T | initialize the force-detach sender. |
| [outbound.rs:525:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L525) | `CEF.force_detach_rx` | T | initialize the shared take-once force-detach receiver. |
| [outbound.rs:526:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L526) | `CEF.write_interruption` | T | initialize the binding's socket write interruption handle. |
| [outbound.rs:538:25](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L538) | `CEF.sm_stream_id` | A/T | share the SM ID cell with owner-serviced resume control. |
| [outbound.rs:539:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L539) | `CEF.binding_ref` | A/T | copy the binding attempt into owner-serviced resume control. |
| [outbound.rs:540:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L540) | `CEF.force_detach_tx` | T | clone the force-detach sender into owner-serviced resume control. |
| [outbound.rs:544:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L544) | `CEF.resume_control` | T | wire the control into the entry's once-only control cell. |
| [outbound.rs:548:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L548) | `CEF.resume_control` | T | read and clone the wired resume-control handle. |
| [outbound.rs:552:15](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L552) | `CE::remote_hosted_for_connection` | O/I/T | delegate remote-mirror construction with a fresh physical reference. |
| [outbound.rs:564:31](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L564) | `CE::new_entry` | O/I/T | construct an active entry with no invented source binding attempt. |
| [outbound.rs:570:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L570) | `CEF.hosting` | O/I | mark the newly constructed owner mirror as remotely hosted. |
| [outbound.rs:592:22](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L592) | `CEF.binding_permit` | A/T | synchronize exact endpoint sealing with permit revocation. |
| [outbound.rs:594:35](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L594) | `CEF.endpoint` | A/T | seal endpoint admission only for the supplied owner and binding. |
| [outbound.rs:597:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L597) | `CEF.binding_permit` | T | notify permit waiters after successful sealing. |
| [outbound.rs:605:36](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L605) | `CEF.binding_ref` | A/T | inspect the entry's binding attempt before actor-specific sealing. |
| [outbound.rs:606:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L606) | `CEF.endpoint` | A/T | seal the actor endpoint for the captured owner and binding. |
| [outbound.rs:606:41](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L606) | `CEF.binding_id` | A | pass the entry's local binding token to actor-specific sealing. |
| [outbound.rs:611:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L611) | `CEF.binding_id` | A | read this entry's local binding token for exact identity comparison. |
| [outbound.rs:611:28](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L611) | `CEF.binding_id` | A | read the other entry's local binding token for exact identity comparison. |
| [outbound.rs:612:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L612) | `CEF.connection_ref` | A/T | read this entry's physical connection identity for comparison. |
| [outbound.rs:612:39](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L612) | `CEF.connection_ref` | A/T | read the other entry's physical connection identity for comparison. |
| [outbound.rs:613:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L613) | `CEF.binding_ref` | A/T | read this entry's binding attempt for comparison. |
| [outbound.rs:613:36](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L613) | `CEF.binding_ref` | A/T | read the other entry's binding attempt for comparison. |
| [outbound.rs:614:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L614) | `CEF.binding_permit` | A/T | compare this entry's permit allocation identity. |
| [outbound.rs:614:51](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L614) | `CEF.binding_permit` | A/T | compare the other entry's permit allocation identity. |
| [outbound.rs:615:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L615) | `CEF.endpoint` | A/T | compare this entry's endpoint binding identity. |
| [outbound.rs:615:44](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L615) | `CEF.endpoint` | A/T | compare the other entry's endpoint binding identity. |
| [outbound.rs:619:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L619) | `CEF.endpoint` | T | read the endpoint's optional actor custody handle. |
| [outbound.rs:636:12](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L636) | `CEF.binding_id` | A | compare the entry's binding token before endpoint recovery. |
| [outbound.rs:636:40](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L636) | `CEF.binding_ref` | A/T | compare the entry's binding attempt before endpoint recovery. |
| [outbound.rs:639:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L639) | `CEF.endpoint` | T | move out the endpoint for exact sealed-and-empty recovery. |
| [outbound.rs:654:12](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L654) | `CEF.binding_id` | A | compare the entry's binding token before receiver-empty recovery. |
| [outbound.rs:654:40](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L654) | `CEF.binding_ref` | A/T | compare the entry's binding attempt before receiver-empty recovery. |
| [outbound.rs:657:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L657) | `CEF.endpoint` | T | move out the endpoint for exact sealed-and-receiver-empty recovery. |
| [outbound.rs:665:10](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L665) | `CEF.connection_ref` | A/T | return a reference to the entry's physical connection identity. |
| [outbound.rs:672:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L672) | `CEF.binding_ref` | A/T | return a reference to the optional binding attempt. |
| [outbound.rs:678:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L678) | `CEF.local_io_relinquishment` | T | return a reference to the optional local I/O witness. |
| [outbound.rs:694:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L694) | `CEF.hosting` | A/I | read hosting to answer whether the socket is local. |
| [outbound.rs:702:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L702) | `CEF.terminal_ordering_retry` | A/T | atomically claim the per-entry terminal-ordering retry slot. |
| [outbound.rs:705:61](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L705) | `CEF.terminal_ordering_retry` | A/T | share the retry flag with the lease that releases it on drop. |
| [outbound.rs:713:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L713) | `CEF.force_detach_tx` | T | clone the force-detach sender. |
| [outbound.rs:725:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L725) | `CEF.binding_ref` | A/T | inspect the entry's binding attempt before an exact detach offer. |
| [outbound.rs:726:25](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L726) | `CE::try_force_detach_raw` | T | fall back to the raw detach offer when no local binding exists. |
| [outbound.rs:728:18](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L728) | `CE::is_locally_hosted` | A/I | check local hosting before using the exact detach path. |
| [outbound.rs:732:25](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L732) | `CE::try_force_detach_raw` | T | fall back to a raw offer when JID or binding verification fails. |
| [outbound.rs:734:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L734) | `CEF.resume_control` | T | read owner-serviced resume control before the exact detach offer. |
| [outbound.rs:735:25](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L735) | `CE::try_force_detach_raw` | T | fall back to a raw offer when socket control is not wired. |
| [outbound.rs:738:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L738) | `CEF.binding_permit` | A/T | serialize permit revocation with a verified detach reservation. |
| [outbound.rs:739:34](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L739) | `CEF.force_detach_tx` | T | try to reserve force-detach channel capacity. |
| [outbound.rs:741:25](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L741) | `CE::try_force_detach_raw` | T | fall back to a raw offer when exact socket-control matching fails. |
| [outbound.rs:752:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L752) | `CEF.binding_permit` | T | notify permit waiters after the verified reservation. |
| [outbound.rs:761:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L761) | `CEF.force_detach_tx` | T | make the raw nonblocking force-detach channel offer. |
| [outbound.rs:785:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L785) | `CE::try_force_detach_and_interrupt_after_offer` | A/T | delegate interrupting detach through the post-offer helper. |
| [outbound.rs:793:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L793) | `CE::try_force_detach_and_revoke` | A/T | reserve detach and revoke the pre-write permit before interrupting. |
| [outbound.rs:796:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L796) | `CEF.binding_ref` | A/T | obtain the binding attempt that the successful local offer proved. |
| [outbound.rs:799:15](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L799) | `CEF.write_interruption` | A/T | interrupt socket I/O only if the captured binding remains current. |
| [outbound.rs:814:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L814) | `CE::try_force_detach_and_revoke_after_reserve` | A/T | delegate detach reservation and revocation to its helper. |
| [outbound.rs:822:29](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L822) | `CEF.binding_ref` | A/T | require a local binding attempt for the detach request. |
| [outbound.rs:829:33](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L829) | `CEF.binding_permit` | A/T | revoke the pre-write permit only when reservation succeeds. |
| [outbound.rs:831:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L831) | `CEF.force_detach_tx` | T | try to reserve force-detach channel capacity. |
| [outbound.rs:843:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L843) | `CEF.binding_permit` | T | notify permit waiters after successful detach reservation. |
| [outbound.rs:877:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L877) | `CEF.force_detach_rx` | T | take the shared force-detach receiver at most once. |
| [outbound.rs:883:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L883) | `CEF.binding_id` | A | return the entry's local binding token. |
| [outbound.rs:888:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L888) | `CEF.carbons_enabled` | A/I | load the entry's carbons opt-in. |
| [outbound.rs:893:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L893) | `CEF.presence_available` | A/I | load the entry's presence-availability flag. |
| [outbound.rs:898:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L898) | `CEF.presence_priority` | A/I | load the entry's advertised presence priority. |
| [outbound.rs:906:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L906) | `CEF.offline_flushed` | A | atomically claim the once-per-entry offline-flush flag. |
| [outbound.rs:917:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L917) | `CEF.pending_subscribes_flushed` | A | atomically claim the once-per-entry subscription-flush flag. |
| [outbound.rs:932:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L932) | `CEF.offline_flushed` | A | reset the offline-flush flag for a later retry. |
| [outbound.rs:941:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L941) | `CEF.occupancy_session` | A/I | lock and read the optional occupancy generation. |
| [outbound.rs:945:32](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L945) | `CEF.sm_stream_id` | A/I | lock and write the entry's SM stream ID without updating the reverse index. |
| [outbound.rs:956:9](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L956) | `CEF.sm_stream_id` | A/I | lock and read the optional SM stream ID. |
| [outbound.rs:992:12](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L992) | `CEF::TYPE` | — | provisional-wrapper field type; no runtime action at this span. |
| [outbound.rs:998:12](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L998) | `CEF::TYPE` | — | actor-provisional-wrapper field type; no runtime action at this span. |
| [outbound.rs:1008:14](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1008) | `CE::binding_ref` | A/T | read the wrapper entry's binding attempt before sealing. |
| [outbound.rs:1009:47](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1009) | `CE::seal_binding` | A/T | seal the wrapper entry only for its exact binding. |
| [outbound.rs:1009:71](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1009) | `CE::binding_id` | A | supply the wrapper entry's local binding token for sealing. |
| [outbound.rs:1013:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1013) | `CE::connection_ref` | A/T | expose the wrapper entry's physical connection identity. |
| [outbound.rs:1017:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1017) | `CE::binding_ref` | A/T | expose the wrapper entry's optional binding attempt. |
| [outbound.rs:1021:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1021) | `CE::binding_id` | A | expose the wrapper entry's local binding token. |
| [outbound.rs:1025:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1025) | `CE::actor_custody` | T | expose the wrapper entry's optional actor custody handle. |
| [outbound.rs:1033:61](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1033) | `CEF::TYPE` | — | comparison parameter type; no runtime action at this span. |
| [outbound.rs:1034:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1034) | `CE::same_exact_local_binding` | A/T | compare the wrapper entry against another exact local binding. |
| [outbound.rs:1046:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L1046) | `CE::binding_id` | A | expose the wrapper entry's local binding token. |

| [owner_update.rs:2:20](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L2) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [owner_update.rs:2:37](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L2) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [owner_update.rs:167:25](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L167) | `CEF.carbons_enabled` | A/I/O | store the remote carbons update on the owner mirror after currency checks. |
| [owner_update.rs:187:21](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L187) | `CEF.roster_interested` | A/I/O | store remote roster interest on the owner mirror after currency checks. |
| [owner_update.rs:208:25](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L208) | `CEF.blocklist_interested` | A/I/O | store remote blocklist interest on the owner mirror after currency checks. |
| [owner_update.rs:253:16](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L253) | `CRF::TYPE` | — | helper parameter type; no runtime action at this span. |
| [owner_update.rs:256:13](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L256) | `CEF::TYPE` | — | helper return type; no runtime action at this span. |
| [owner_update.rs:267:20](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L267) | `CE::binding_id` | A | compare the actor entry token with the remote registration owner. |
| [owner_update.rs:270:41](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L270) | `CR::entry_if_owner` | A/I | fetch the registry entry only if the remote registration owner remains current. |
| [owner_update.rs:274:10](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L274) | `CEF.carbons_enabled` | A/I/O | compare the registry mirror's shared carbons allocation with the actor copy. |
| [owner_update.rs:275:10](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L275) | `CEF.carbons_enabled` | A/I/O | compare the actor copy's shared carbons allocation with the registry mirror. |
| [types.rs:2:42](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L2) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [types.rs:150:17](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L150) | `CEF::TYPE` | — | snapshot-constructor parameter type; no runtime action at this span. |
| [types.rs:154:35](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L154) | `CE::connection_ref` | A/T/O | copy physical connection identity into the remote state snapshot. |
| [types.rs:155:38](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L155) | `CE::occupancy_session` | A/I/O | read occupancy generation into the remote state snapshot. |
| [types.rs:156:30](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L156) | `CEF.carbons_enabled` | A/I/O | read carbons opt-in into the remote state snapshot. |
| [types.rs:157:32](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L157) | `CEF.roster_interested` | A/I/O | read roster interest into the remote state snapshot. |
| [types.rs:158:35](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L158) | `CEF.blocklist_interested` | A/I/O | read blocklist interest into the remote state snapshot. |
| [types.rs:159:33](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L159) | `CEF.presence_available` | A/I/O | read presence availability into the remote state snapshot. |
| [types.rs:160:32](../../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L160) | `CEF.presence_priority` | A/I/O | read presence priority into the remote state snapshot. |
| [cleanup.rs:908:70](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L908) | `CR::presence_state` | A/I | read the full presence snapshot before building a detached-session snapshot. |
| [cleanup.rs:913:14](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L913) | `CR::entry_if_owner` | A/I | capture only the entry still owned by the disconnecting socket. |
| [cleanup.rs:930:40](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L930) | `CE::is_presence_available` | A/I | read the captured entry's availability for the detached snapshot. |
| [cleanup.rs:984:42](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L984) | `CE::presence_priority` | A/I | read entry priority when no full presence snapshot is available. |
| [cleanup.rs:992:41](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L992) | `CEF.pending_subscribes_flushed` | A | read the subscription-flush claim into the detached-session snapshot. |
| [cleanup.rs:1132:26](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1132) | `CR::unregister_if_owner` | A/I/T | remove only the captured owner after a successful detach. |
| [cleanup.rs:1353:26](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1353) | `CR::unregister_if_owner` | A/I/T | remove only the captured owner after detach persistence fails. |
| [cleanup.rs:1496:26](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1496) | `CR::entry` | I | check for any replacement route before clearing shared capability state. |
| [cleanup.rs:1521:14](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1521) | `CR::unregister_if_owner` | A/I/T | remove the disconnecting socket's entry only if its owner remains current. |
| [cleanup.rs:1530:52](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1530) | `CE::is_presence_available` | A/I | read availability from the just-removed entry for termination handling. |
| [cleanup.rs:1764:10](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1764) | `CR::prune_completed_bind` | A/T | prune a completed JID bind slot after socket cleanup. |
| [cleanup.rs:1848:18](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1848) | `CR::unregister_if_owner` | A/I/T | remove the terminal recovery route only for the captured owner. |
| [cleanup.rs:1849:37](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1849) | `CE::is_presence_available` | A/I | read availability from the removed terminal entry. |
| [cleanup.rs:1907:39](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1907) | `CRF::TYPE` | — | helper parameter type; no runtime action at this span. |
| [cleanup.rs:1911:10](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1911) | `CR::available_resources_for_user` | A/I | enumerate available resources to select a pending-delivery target. |
| [cleanup.rs:1924:18](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1924) | `CR::entry` | I | fetch the current entry for each available-resource candidate. |
| [cleanup.rs:1931:23](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1931) | `CE::is_locally_hosted` | A/I | reject a nonlocal candidate for pending recovery. |
| [cleanup.rs:1932:27](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1932) | `CE::is_presence_available` | A/I | recheck the adopted entry's availability. |
| [cleanup.rs:1933:26](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1933) | `CE::presence_priority` | A/I | recheck the adopted entry's priority is nonnegative. |
| [cleanup.rs:1939:30](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1939) | `CE::binding_id` | A | capture the adopted entry's local owner token. |
| [cleanup.rs:1940:35](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1940) | `CE::sm_stream_id` | A/I | capture the adopted entry's SM stream ID. |
| [cleanup.rs:2022:18](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2022) | `CR::available_resources_for_user` | A/I | enumerate available resources when no local recovery target was found. |
| [cleanup.rs:2025:30](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2025) | `CR::entry` | I | fetch each candidate to determine whether a remote available resource exists. |
| [cleanup.rs:2026:32](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2026) | `CE::is_locally_hosted` | A/I | test whether the candidate is remotely hosted. |
| [cleanup.rs:2027:38](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2027) | `CE::is_presence_available` | A/I | recheck candidate availability for remote recovery handling. |
| [cleanup.rs:2028:38](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2028) | `CE::presence_priority` | A/I | recheck candidate nonnegative priority for remote recovery handling. |
| [cleanup.rs:2055:14](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2055) | `CR::entry_if_owner` | A/I | recapture the chosen target only if its owner remains current. |
| [cleanup.rs:2062:39](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2062) | `CE::try_acquire_terminal_ordering_retry` | A/T | claim the target's single terminal-ordering retry lease. |
| [cleanup.rs:2090:14](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2090) | `CR::entry_if_owner` | A/I | recheck the target owner after the async flush. |
| [cleanup.rs:2125:18](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2125) | `CR::entry_if_owner` | A/I | recapture the current target owner before resetting a blocked flush. |
| [cleanup.rs:2127:23](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2127) | `CE::reset_offline_flush` | A | reset the target's offline-flush flag when further promotion is blocked. |
| [cleanup.rs:2199:43](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2199) | `CR::entry_if_owner` | A/I | fetch the current target owner after a retry attempt. |
| [cleanup.rs:2200:26](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2200) | `CE::is_locally_hosted` | A/I | require the retry target to remain locally hosted. |
| [cleanup.rs:2201:30](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2201) | `CE::is_presence_available` | A/I | require the retry target to remain available. |
| [cleanup.rs:2202:30](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2202) | `CE::presence_priority` | A/I | require the retry target to retain nonnegative priority. |
| [cleanup.rs:2205:31](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2205) | `CE::reset_offline_flush` | A | reset the current entry's offline-flush flag after a transient deferral. |
| [cleanup.rs:2217:40](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2217) | `CR::entry_if_owner` | A/I | fetch the selected replacement only if its owner remains current. |
| [cleanup.rs:2221:49](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2221) | `CE::try_acquire_terminal_ordering_retry` | A/T | claim the replacement's terminal-ordering retry lease. |
| [cleanup.rs:2624:22](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2624) | `CR::unregister_if_owner` | A/I/T | remove the terminal entry only if its owner remains current. |
| [cleanup.rs:2625:41](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2625) | `CE::is_presence_available` | A/I | read availability from the removed terminal entry. |
| [cleanup.rs:2733:77](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2733) | `CR::entry` | I | check whether a replacement route appeared during terminal recovery. |
| [cleanup.rs:2820:10](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2820) | `CR::entry` | I | fetch a possible replacement before broadcasting unavailable presence. |
| [cleanup.rs:2821:36](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2821) | `CE::is_presence_available` | A/I | suppress the unavailable broadcast if the replacement is available. |
| [cleanup.rs:3843:14](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L3843) | `CR::entry_if_owner` | A/I | check whether the replacement owner still occupies the route. |
| [cleanup.rs:3861:14](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L3861) | `CR::unregister_if_sm_stream_id` | A/I/T | remove an invalidated entry only if its SM session ID matches. |
| [cleanup.rs:3869:28](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L3869) | `CE::binding_id` | A/O | read the removed entry's binding token for actor-mirror unregister. |
| [cleanup.rs:3872:83](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L3872) | `CE::binding_id` | A/O | read the removed entry's binding token for remote-mirror unregister. |
| [cleanup.rs:3886:14](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L3886) | `CR::entry` | I | fetch a possible unrelated replacement before clearing capability state. |
| [cleanup.rs:3889:22](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L3889) | `CE::sm_stream_id` | A/I | read the replacement's SM ID to distinguish it from the detached session. |
| [binding.rs:1:13](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L1) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [binding.rs:1:30](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L1) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [binding.rs:32:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L32) | `CEF::TYPE` | — | lifecycle field type; no runtime action at this span. |
| [binding.rs:105:41](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L105) | `CE::binding_id` | A | read the lifecycle entry's token before retaining actor retirement. |
| [binding.rs:109:46](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L109) | `CE::binding_id` | A | compare each pending actor-retirement token for deduplication. |
| [binding.rs:125:41](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L125) | `CE::binding_id` | A | read the completed lifecycle entry's token. |
| [binding.rs:128:49](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L128) | `CE::binding_id` | A | remove pending actor retirements whose token matches the completed entry. |
| [binding.rs:143:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L143) | `CEF::TYPE` | — | lifecycle publish parameter type; no runtime action at this span. |
| [binding.rs:184:6](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L184) | `CRF::TYPE` | — | implementation receiver type; no runtime action at this span. |
| [binding.rs:202:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L202) | `CRF.bind_slots` | A/T | create or obtain the per-JID bind slot for a nonblocking lock attempt. |
| [binding.rs:209:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L209) | `CRF.bind_slots` | A/T | clone the shared bind-slot map into the acquired guard. |
| [binding.rs:220:26](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L220) | `CRF.bind_slots` | A/T | read the JID bind slot for a nonblocking lifecycle probe. |
| [binding.rs:239:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L239) | `CRF.bind_slots` | A/T | create or obtain the per-JID bind slot for an async lock. |
| [binding.rs:261:20](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L261) | `CRF.bind_slots` | A/T | clone the shared bind-slot map into the acquired guard. |
| [binding.rs:268:16](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L268) | `CRF.bind_slots` | A/T | pass the bind-slot map to completed-slot pruning. |
| [delivery.rs:40:53](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L40) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [delivery.rs:59:21](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L59) | `CE::try_send_archive_ordered` | A/T | invoke archive-ordered enqueue for the selected actor resource. |
| [delivery.rs:108:19](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L108) | `CE::is_locally_hosted` | A/I | reject a remote mirror in the dark local-custody dispatch path. |
| [delivery.rs:113:27](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L113) | `CE::binding_id` | A | capture the selected entry's local binding token for retry evidence. |
| [delivery.rs:114:39](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L114) | `CE::binding_ref` | A/T | capture its exact local binding attempt for retry evidence. |
| [delivery.rs:120:21](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L120) | `CE::try_send_archive_ordered_with_refusal` | A/T | invoke refusal-preserving archive enqueue for local custody. |
| [delivery.rs:207:40](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L207) | `CE::is_presence_available` | A/I | filter actor resources by available presence for bare-JID selection. |
| [delivery.rs:208:45](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L208) | `CE::presence_priority` | A/I | read priority for each available actor resource. |
| [delivery.rs:356:25](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L356) | `CEF::TYPE` | — | actor reply type; no runtime action at this span. |

| [channels.rs:52:28](../../../server/crates/waddle-server/src/admin/channels.rs#L52) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [channels.rs:365:30](../../../server/crates/waddle-server/src/admin/channels.rs#L365) | `CRF::TYPE` | — | registration parameter type; no runtime action at this span. |
| [channels.rs:647:22](../../../server/crates/waddle-server/src/admin/channels.rs#L647) | `CRF::TYPE` | — | handler parameter type; no runtime action at this span. |
| [channels.rs:691:22](../../../server/crates/waddle-server/src/admin/channels.rs#L691) | `CRF::TYPE` | — | handler parameter type; no runtime action at this span. |
| [channels.rs:722:22](../../../server/crates/waddle-server/src/admin/channels.rs#L722) | `CRF::TYPE` | — | handler parameter type; no runtime action at this span. |
| [channels.rs:800:22](../../../server/crates/waddle-server/src/admin/channels.rs#L800) | `CRF::TYPE` | — | handler parameter type; no runtime action at this span. |
| [channels.rs:834:22](../../../server/crates/waddle-server/src/admin/channels.rs#L834) | `CRF::TYPE` | — | handler parameter type; no runtime action at this span. |
| [channels.rs:2195:19](../../../server/crates/waddle-server/src/admin/channels.rs#L2195) | `CRF::TYPE` | — | operation parameter type; no runtime action at this span. |
| [channels.rs:2445:19](../../../server/crates/waddle-server/src/admin/channels.rs#L2445) | `CRF::TYPE` | — | operation parameter type; no runtime action at this span. |
| [channels.rs:2799:19](../../../server/crates/waddle-server/src/admin/channels.rs#L2799) | `CRF::TYPE` | — | broadcast parameter type; no runtime action at this span. |
| [channels.rs:2820:29](../../../server/crates/waddle-server/src/admin/channels.rs#L2820) | `CR::try_send_to` | T | try nonblocking delivery of departure presence to the leaving real JID. |
| [channels.rs:2833:29](../../../server/crates/waddle-server/src/admin/channels.rs#L2833) | `CR::try_send_to` | T | try nonblocking delivery of departure presence to an occupant JID. |
| [channels.rs:2838:19](../../../server/crates/waddle-server/src/admin/channels.rs#L2838) | `CRF::TYPE` | — | broadcast parameter type; no runtime action at this span. |
| [channels.rs:2845:33](../../../server/crates/waddle-server/src/admin/channels.rs#L2845) | `CR::try_send_to` | T | try nonblocking delivery of a group-DM configuration message. |
| [channels.rs:2953:19](../../../server/crates/waddle-server/src/admin/channels.rs#L2953) | `CRF::TYPE` | — | broadcast parameter type; no runtime action at this span. |
| [channels.rs:2979:18](../../../server/crates/waddle-server/src/admin/channels.rs#L2979) | `CR::send_to` | T | await delivery admission for an admin configuration message. |
| [channels.rs:3435:19](../../../server/crates/waddle-server/src/admin/channels.rs#L3435) | `CRF::TYPE` | — | broadcast parameter type; no runtime action at this span. |
| [channels.rs:3440:14](../../../server/crates/waddle-server/src/admin/channels.rs#L3440) | `CR::send_to` | T | await delivery admission for a presence update. |
| [channels.rs:3447:19](../../../server/crates/waddle-server/src/admin/channels.rs#L3447) | `CRF::TYPE` | — | operation parameter type; no runtime action at this span. |
| [channels.rs:4108:19](../../../server/crates/waddle-server/src/admin/channels.rs#L4108) | `CRF::TYPE` | — | operation parameter type; no runtime action at this span. |
| [channels.rs:4954:19](../../../server/crates/waddle-server/src/admin/channels.rs#L4954) | `CRF::TYPE` | — | operation parameter type; no runtime action at this span. |
| [channels.rs:5103:19](../../../server/crates/waddle-server/src/admin/channels.rs#L5103) | `CRF::TYPE` | — | operation parameter type; no runtime action at this span. |
| [channels.rs:5149:19](../../../server/crates/waddle-server/src/admin/channels.rs#L5149) | `CRF::TYPE` | — | operation parameter type; no runtime action at this span. |
| [channels.rs:5327:14](../../../server/crates/waddle-server/src/admin/channels.rs#L5327) | `CR::send_to` | T | await delivery admission for a kick-related presence update. |
| [local_claims.rs:39:5](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L39) | `CRF::TYPE` | — | type import; no runtime action at this span. |
| [local_claims.rs:82:39](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L82) | `CRF::TYPE` | — | bridge field type; no runtime action at this span. |
| [local_claims.rs:107:58](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L107) | `CRF::TYPE` | — | wire parameter type; no runtime action at this span. |
| [local_claims.rs:169:48](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L169) | `CR::sm_stream_owner` | I | look up a candidate JID from the SM reverse index during demotion. |
| [local_claims.rs:170:49](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L170) | `CR::entry` | I | fetch the candidate's current entry during demotion. |
| [local_claims.rs:171:32](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L171) | `CE::sm_stream_id` | A/I | recheck the entry SM ID against the candidate session. |
| [local_claims.rs:195:43](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L195) | `CE::binding_ref` | A/T | copy the captured entry's binding attempt into a detach request. |
| [local_claims.rs:199:26](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L199) | `CE::force_detach_sender` | T | clone the force-detach sender before awaiting a demotion request send. |
| [local_claims.rs:666:39](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L666) | `CRF::TYPE` | — | bridge field type; no runtime action at this span. |
| [local_claims.rs:686:58](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L686) | `CRF::TYPE` | — | wire parameter type; no runtime action at this span. |
| [local_claims.rs:766:31](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L766) | `CRF::TYPE` | — | resource-enumeration parameter type; no runtime action at this span. |
| [local_claims.rs:770:14](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L770) | `CR::list_connections` | I | enumerate route JIDs to find resources for a demoted bare JID. |
| [local_claims.rs:804:51](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L804) | `CR::entry` | I | fetch current entries for the resources to be force-detached. |
| [local_claims.rs:827:55](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L827) | `CEF::TYPE` | — | demotion parameter type; no runtime action at this span. |
| [local_claims.rs:833:31](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L833) | `CE::binding_id` | A | capture the entry's local owner token before the detach request. |
| [local_claims.rs:837:39](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L837) | `CE::binding_ref` | A/T | copy the entry's exact binding attempt into the detach request. |
| [local_claims.rs:842:25](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L842) | `CE::try_force_detach_for_jid` | A/T | try an identity-aware force-detach offer for the resource. |
| [local_claims.rs:904:22](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L904) | `CR::unregister_if_owner` | A/I/T | remove the demoted route only for the captured owner after acknowledgment. |
| [local_claims.rs:966:44](../../../server/crates/waddle-server/src/clustering/local_claims.rs#L966) | `CR::list_connections` | I/T | enumerate transport routes during terminal sweep. |
| [remote_socket.rs:48:14](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L48) | `CR::entry_if_owner` | A/I | fetch the socket entry only if the registration owner remains current. |
| [remote_socket.rs:49:40](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L49) | `CE::connection_ref` | A/T | compare the entry's physical connection identity against the registration. |
| [remote_socket.rs:514:18](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L514) | `CR::entry_if_owner` | A/I | capture the registered socket owner before remote frame admission. |
| [remote_socket.rs:520:32](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L520) | `CE::sm_stream_id` | A/I | read the captured socket entry's SM stream ID. |
| [remote_socket.rs:569:54](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L569) | `CR::try_send_outbound_if_owner` | A/T | invoke owner-checked enqueue of an ingress-obligation remote frame. |
| [remote_socket.rs:586:52](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L586) | `CR::try_send_outbound_if_owner` | A/T | invoke owner-checked enqueue of an ordinary remote frame. |
| [remote_socket.rs:636:52](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L636) | `CR::try_send_outbound_if_owner` | A/T | invoke owner-checked enqueue of a write-acceptance remote frame. |
| [registration.rs:366:32](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L366) | `CR::entry_if_owner` | A/I | fetch the resume attempt's entry only if the owner remains current. |
| [registration.rs:368:42](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L368) | `CE::binding_ref` | A/T | capture the current entry's exact binding attempt. |
| [registration.rs:369:26](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L369) | `CE::seal_binding` | A/T | seal the exact resume binding before route removal. |
| [registration.rs:370:53](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L370) | `CR::unregister_if_owner` | A/I/T | remove the route only if the resume owner remains current. |
| [registration.rs:376:38](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L376) | `CE::into_socket_outbound_endpoint_if_receiver_empty` | A/T | recover the sealed endpoint if the paired receiver is empty. |
| [registration.rs:380:45](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L380) | `CE::into_socket_outbound_endpoint_if_empty` | A/T | recover the sealed endpoint if its queue and reservations are empty. |
| [registration.rs:473:14](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L473) | `CR::prune_completed_bind` | A/T | prune a completed bind slot after resume reset. |
| [registration.rs:519:10](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L519) | `CR::unregister_if_owner` | A/I/T | remove only the registered resume attempt's owner on closure. |
| [user_registry.rs:22:5](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L22) | `CEF::TYPE` | — | type import; no runtime action at this span. |
| [user_registry.rs:79:33](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L79) | `CEF::TYPE` | — | hard-demotion resource-map value type; no runtime action at this span. |
| [user_registry.rs:402:18](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L402) | `CEF::TYPE` | — | type-qualified method reference; no separate runtime action. |
| [user_registry.rs:402:35](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L402) | `CE::binding_id` | A | pass the entry binding-ID accessor to map a pending unregister entry. |
| [user_registry.rs:787:42](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L787) | `CE::binding_ref` | A/T | copy the stale resource's binding attempt into a detach request. |
| [user_registry.rs:791:28](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L791) | `CE::try_force_detach_for_jid` | A/T | offer detach for the stale resource; exact local control can revoke its permit on reservation, but unmatched cases use a raw offer and the socket validates the request. |
| [user_registry.rs:1087:16](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1087) | `CEF::TYPE` | — | actor message field type; no runtime action at this span. |
| [user_registry.rs:1152:16](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1152) | `CEF::TYPE` | — | actor message field type; no runtime action at this span. |
| [user_registry.rs:1511:16](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1511) | `CEF::TYPE` | — | actor message field type; no runtime action at this span. |

The historical-reference table stops at its stated 739 identities. The
structured export additionally records individually reviewed current-source
sites; every refreshed registry/entry reference absent from that export remains
`manual-review-required`. Declaration summaries and selected-edge discussion
do not by themselves classify a reference.
