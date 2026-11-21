# Server metrics reference

Every OpenTelemetry metric the Rust server declares, read from the source
at the commit that last changed this file. Metric names are the
OpenTelemetry instrument names. The server exports them over OTLP;
Grafana Alloy forwards OTLP unchanged to Grafana Cloud, which stores them
under Prometheus names.

The "derived Prometheus name" column applies the documented OTLP to
Prometheus translation (`UnderscoreEscapingWithSuffixes`): dots become
underscores, a unit of `s`, `ms` or `By` appends `_seconds`,
`_milliseconds` or `_bytes`, a unit in braces appends nothing, counters
append `_total`, and histograms expose `_bucket`, `_sum` and `_count`. These
names are derived, not observed, except where
`infrastructure/waddle.cloud/rules/tests/push_otlp_contract.py` runs the
real translator: the push queue gauges and the clustering instruments its
contracts list.

Macro attributes come from the allowlist in
`server/crates/waddle-xmpp/src/telemetry/attributes.rs`; instruments
created directly set their attributes at each call site. No metric carries
a JID, a stream id, a node id or any other identifier. Source paths below
are relative to `server/crates/`.

## What the umbrella's list maps to

| Concern | Metrics |
|---|---|
| Admission | `waddle.ingress.entry.transactions`, `waddle.connection_actor.mailbox.rejected`, `waddle.muc.admission.denied`, `waddle.auth.failures`, `waddle.session.init.failed` |
| Custody | `ingress.effects.unresolved`, `xmpp.push.queue.parked`, `xmpp.push.queue.unresolved_failed`, `waddle.room_effect_outbox.depth`, `waddle.call.teardown.outbox.depth`, `waddle.muc.destroy_completion.depth`, `xmpp.remote_owner_mirror.pending`, the `xmpp.pending_delivery.*` counters |
| Completion uncertainty | `waddle.relay.ordered.outcomes{outcome="uncertain"}`, `waddle.muc.remote_dispatch.outcomes{outcome="uncertain"}`, `xmpp.push.failure_class`, `waddle.clustering.claim.cancellations`, `waddle.clustering.fence.results{result="cancelled"}` |
| Fencing rejection | `waddle.clustering.fence.results`, `waddle.clustering.keypair_lease.outcomes{outcome="fencing_lost"}`, `waddle.clustering.ingress_append.authorization_failed`, `xmpp.sm.detached.append.rejected`, `ingress.decisions` |
| Mailbox bytes, depth and age | `waddle.connection_actor.mailbox.depth`, `.bytes`, `.oldest_age`; `xmpp.actor.mailbox.depth`, `xmpp.actor.mailbox.latency`, `waddle.actor.message.queue_wait` |
| Actor and transport progress | `waddle.actor.message.handle.duration`, `waddle.actor.started`, `waddle.actor.stopped`, `waddle.connection_actor.turn.duration`, `waddle.connection_actor.retirements`, `waddle.io.pump.exits`, `waddle.io.custodian.retirements`, `waddle.io.writer.blocked.duration`, `waddle.janitor.sweeps` |
| Pending acceptance | `waddle.muc.remote_acceptance.pending`, `waddle.muc.remote_acceptance.oldest_pending_age` |
| Replay | `xmpp.sm.resume.results`, `xmpp.sm.resume.replayed`, `xmpp.sm.unacked.depth`, `xmpp.sm.unacked_evicted`, `xmpp.sm.handled_progress` |
| Recovery | `ingress.maintenance.*`, `ingress.gc.*`, `ingress.recovery.reachability_blocked`, `xmpp.remote_owner_mirror.attempts`, `waddle.muc.destroy_completion.outcomes` |
| Restricted database runtime | `waddle.restricted_runtime.startup.refusals`, `waddle.restricted_runtime.attach.failures` |

## Authority, custody and progress metrics

Declared in `waddle-xmpp/src/telemetry/authority.rs`, except the
`xmpp.account_coordinator.*` instruments in
`waddle-xmpp/src/telemetry/account_coordinator.rs`, each behind a typed
recording function. Failure outcomes are created at zero at startup;
progress outcomes are not.

| Metric | Type | Unit | Attributes | Derived Prometheus name | Meaning |
|---|---|---|---|---|---|
| `waddle.clustering.keypair_lease.outcomes` | counter | `{operation}` | `outcome` | `waddle_clustering_keypair_lease_outcomes_total` | Keypair-slot lease operations, by outcome. |
| `waddle.connection_actor.mailbox.bytes` | histogram | `By` | `lane` | `waddle_connection_actor_mailbox_bytes_bytes_bucket / _sum / _count` | Bytes retained by a connection actor mailbox lane when sampled. |
| `waddle.connection_actor.mailbox.depth` | histogram | `{message}` | `lane` | `waddle_connection_actor_mailbox_depth_bucket / _sum / _count` | Messages queued in a connection actor mailbox lane when sampled. |
| `waddle.connection_actor.mailbox.oldest_age` | histogram | `s` | `lane` | `waddle_connection_actor_mailbox_oldest_age_seconds_bucket / _sum / _count` | Age of the oldest message in a connection actor mailbox lane when sampled; zero when empty. |
| `waddle.connection_actor.mailbox.rejected` | counter | `{message}` | `lane`, `reason` | `waddle_connection_actor_mailbox_rejected_total` | Messages a connection actor mailbox refused, by lane and reason. |
| `waddle.connection_actor.retirements` | counter | `{actor}` | `cause` | `waddle_connection_actor_retirements_total` | Connection actors that retired, by cause. |
| `waddle.connection_actor.turn.duration` | histogram | `s` | `event` | `waddle_connection_actor_turn_duration_seconds_bucket / _sum / _count` | Duration of one connection actor turn, by the event it handled. |
| `waddle.ingress.entry.preparation.duration` | histogram | `s` | `authority` | `waddle_ingress_entry_preparation_duration_seconds_bucket / _sum / _count` | Time from an ingress entry request to its prepared transaction or refusal, by authority. |
| `waddle.ingress.entry.transactions` | counter | `{transaction}` | `authority`, `outcome` | `waddle_ingress_entry_transactions_total` | Ingress transactions requested through the typed entry, by authority and outcome. |
| `waddle.io.custodian.retirements` | counter | `{retirement}` | `cause` | `waddle_io_custodian_retirements_total` | Transport custodians that retired their socket and pumps, by cause. |
| `waddle.io.pump.exits` | counter | `{exit}` | `pump`, `cause` | `waddle_io_pump_exits_total` | Supervised transport pumps that ended, by pump and cause. |
| `waddle.io.writer.blocked.duration` | histogram | `s` | — | `waddle_io_writer_blocked_duration_seconds_bucket / _sum / _count` | Time a transport write spent blocked on the peer before it completed or was abandoned. |
| `waddle.muc.destroy_completion.depth` | gauge | `{completion}` | — | `waddle_muc_destroy_completion_depth` | Room-destroy completions waiting in the durable queue. |
| `waddle.muc.destroy_completion.oldest_age` | gauge | `s` | — | `waddle_muc_destroy_completion_oldest_age_seconds` | Age of the oldest waiting room-destroy completion; zero when the queue is empty. |
| `waddle.muc.destroy_completion.outcomes` | counter | `{completion}` | `outcome` | `waddle_muc_destroy_completion_outcomes_total` | Leased room-destroy completions, by outcome. |
| `waddle.muc.remote_acceptance.oldest_pending_age` | histogram | `s` | — | `waddle_muc_remote_acceptance_oldest_pending_age_seconds_bucket / _sum / _count` | Age of the oldest remote-room submission awaiting acceptance when sampled; zero with none. |
| `waddle.muc.remote_acceptance.pending` | histogram | `{submission}` | — | `waddle_muc_remote_acceptance_pending_bucket / _sum / _count` | Remote-room submissions awaiting the owner's acceptance when sampled. |
| `waddle.muc.remote_dispatch.outcomes` | counter | `{submission}` | `outcome` | `waddle_muc_remote_dispatch_outcomes_total` | Submissions to a room owned by another node, by outcome. |
| `waddle.relay.ordered.outcomes` | counter | `{delivery}` | `outcome` | `waddle_relay_ordered_outcomes_total` | Ordered relay deliveries as their sender saw them end, by outcome. |
| `waddle.relay.ordered.queue.depth` | histogram | `{delivery}` | — | `waddle_relay_ordered_queue_depth_bucket / _sum / _count` | Deliveries queued on an ordered relay channel when sampled. |
| `waddle.relay.ordered.queue.oldest_age` | histogram | `s` | — | `waddle_relay_ordered_queue_oldest_age_seconds_bucket / _sum / _count` | Age of the oldest delivery queued on an ordered relay channel when sampled; zero when empty. |
| `waddle.restricted_runtime.attach.failures` | counter | `{failure}` | `store` | `waddle_restricted_runtime_attach_failures_total` | Store attachments that failed verification under the restricted database role, by store. |
| `waddle.restricted_runtime.startup.refusals` | counter | `{refusal}` | `stage` | `waddle_restricted_runtime_startup_refusals_total` | Starts under the restricted database role that refused to serve, by stage. |
| `xmpp.account_coordinator.mailbox.rejected` | counter | `{message}` | `reason` | `xmpp_account_coordinator_mailbox_rejected_total` | Messages an account coordinator mailbox refused, by reason. |
| `xmpp.account_coordinator.operations` | counter | `{operation}` | `mutation`, `outcome` | `xmpp_account_coordinator_operations_total` | Account coordinator operations by mutation and bounded outcome. |
| `xmpp.account_coordinator.slots` | histogram | `{slot}` | — | `xmpp_account_coordinator_slots_bucket / _sum / _count` | Resource slots held by an account, sampled after each committed mutation. |
| `xmpp.sm.enable.results` | counter | `{request}` | `outcome` | `xmpp_sm_enable_results_total` | XEP-0198 enable requests, by outcome. |
| `xmpp.sm.resume.replayed` | histogram | `{stanza}` | — | `xmpp_sm_resume_replayed_bucket / _sum / _count` | Unacknowledged stanzas replayed by a successful XEP-0198 resumption. |
| `xmpp.sm.unacked.depth` | histogram | `{stanza}` | — | `xmpp_sm_unacked_depth_bucket / _sum / _count` | Stanzas still unacknowledged on a stream after an acknowledgement was processed. |

Which of these are recorded today:

| Metric | Recorded |
|---|---|
| `waddle.ingress.entry.transactions`, `waddle.ingress.entry.preparation.duration` | Yes, in `begin_ingress_entry` (`waddle-server/src/ingress_substrate/typed_entry.rs`) |
| `waddle.clustering.keypair_lease.outcomes` | Yes, in `waddle-server/src/clustering/lease.rs`: `acquired`, `exhausted`, `fencing_lost` and `released` |
| `waddle.restricted_runtime.startup.refusals` | Yes, in `waddle-server/src/main.rs`, which flushes metrics before the process exits |
| `waddle.restricted_runtime.attach.failures` | Yes, at each attachment under the attested pool in `server/http.rs`, `server/lifecycle.rs`, `clustering/node.rs` and `clustering/swarm.rs`. `room_observation`, `webhook_deliveries` and the claim authority's own attachment have no call site. The process exits after a failed attachment and `main.rs` flushes first |
| `waddle.muc.destroy_completion.outcomes` | `completed`, `retried` and `failed`, in `drain_destroy_completions` and `drain_persisted_destroy_completions`. `lease_taken_over` has no call site: the claim does not tell a free row from a lapsed lease |
| `waddle.muc.destroy_completion.depth`, `.oldest_age` | Yes, by the destroy-completion janitor after each successful reading of the queue |
| `waddle.janitor.sweeps{janitor="destroy_completion"}` | Yes, as a heartbeat of the destroy-completion janitor |
| `waddle.connection_actor.mailbox.depth` | declared, not yet recorded (#2046); `record_actor_mailbox` has no production caller |
| `waddle.connection_actor.mailbox.bytes` | declared, not yet recorded (#2046); `record_actor_mailbox` has no production caller |
| `waddle.connection_actor.mailbox.oldest_age` | declared, not yet recorded (#2046); `record_actor_mailbox` has no production caller |
| `waddle.io.writer.blocked.duration` | declared, not yet recorded (#2046); `record_io_writer_blocked` has no production caller |
| `waddle.relay.ordered.queue.depth` | declared, not yet recorded (#2046); `record_relay_queue` has no production caller |
| `waddle.relay.ordered.queue.oldest_age` | declared, not yet recorded (#2046); `record_relay_queue` has no production caller |
| `waddle.relay.ordered.outcomes` | zero-registered only; no production increment yet. `record_relay_outcome` has no production caller |
| `waddle.connection_actor.mailbox.rejected` | Yes, in `waddle-server/src/server/routes/websocket/frame.rs` |
| `waddle.connection_actor.turn.duration` | Yes, in `waddle-server/src/server/routes/websocket/connection/live_actor.rs` |
| `waddle.connection_actor.retirements`, `waddle.io.pump.exits`, `waddle.io.custodian.retirements` | Yes, in `waddle-server/src/server/routes/websocket/connection.rs` |
| `waddle.muc.remote_dispatch.outcomes`, `waddle.muc.remote_acceptance.pending`, `waddle.muc.remote_acceptance.oldest_pending_age` | Yes, in `waddle-server/src/ingress/origin_gate.rs` |
| `xmpp.account_coordinator.operations`, `xmpp.account_coordinator.slots` | Yes, in `waddle-server/src/account_coordinator/actor.rs` |
| `xmpp.account_coordinator.mailbox.rejected` | Yes, in `waddle-server/src/account_coordinator/directory.rs` |
| `xmpp.sm.enable.results` | Yes, in `waddle-server/src/server/routes/websocket/stream_management/implementation.rs` and `waddle-server/src/server/routes/websocket/stream_management/durable.rs` |
| `xmpp.sm.resume.replayed` | Yes, in `waddle-server/src/server/routes/websocket/stream_management/durable.rs` |
| `xmpp.sm.unacked.depth` | Yes, in `waddle-server/src/server/routes/websocket/stream_management/implementation.rs` |

## Server-to-server connection metrics

Emitted by the S2S connection manager
(`server/crates/waddle-server/src/s2s/conn/telemetry.rs`), the S2S router
(`server/crates/waddle-server/src/s2s/route/telemetry.rs`), the host of
local rooms for remote occupants and S2S discovery
(`server/crates/waddle-server/src/s2s/discovery.rs`) only when S2S is
enabled, which it is not by default. Attributes are the closed enums in
`server/crates/waddle-xmpp/src/telemetry/attributes/s2s.rs`; no metric
carries a domain, an address or a stream id. None is zero-registered.

| Metric | Type | Unit | Attributes | Derived Prometheus name | Meaning |
|---|---|---|---|---|---|
| `waddle.s2s.connections` | counter | `{connection}` | `direction`, `outcome` | `waddle_s2s_connections_total` | S2S connections that ended, by how they ended. |
| `waddle.s2s.capacity.refusals` | counter | `{connection}` | `direction`, `resource` | `waddle_s2s_capacity_refusals_total` | Sockets, streams, dials and dialback verification connections a connection limit refused, by the limit (`sockets`, `unauthenticated`, `source_address`, `accept_rate`, `handshakes`, `streams`, `peers`, `per_peer`, `peer_streams`, `dial_rate`, `verifications`, `verification_rate`); `peer_streams` is the cap on inbound streams for one directional pair. |
| `waddle.s2s.established` | counter | `{stream}` | `direction`, `transport`, `strength` | `waddle_s2s_established_total` | Streams that completed negotiation over TLS, by how the pair authenticated (`pkix` or `dialback`). |
| `waddle.s2s.negotiation.duration` | histogram | `ms` | `direction` | `waddle_s2s_negotiation_duration_milliseconds_bucket / _sum / _count` | Time from TCP accept or connect to an established stream. |
| `waddle.s2s.streams` | gauge | `{stream}` | `direction` | `waddle_s2s_streams` | Established streams. |
| `waddle.s2s.dialback` | counter | `{exchange}` | `role`, `result` | `waddle_s2s_dialback_total` | Server Dialback exchanges, by this server's role (`initiating`, `receiving`, `authoritative`) and result (`valid`, `invalid`, `error`). |
| `waddle.s2s.discovery` | counter | `{lookup}` | `source` | `waddle_s2s_discovery_total` | Endpoint resolutions, by where the endpoints came from (`configured`, `srv`, `fallback`) or why there were none (`no_service`, `lookup_failed`, `blocked`). |
| `waddle.s2s.route.unusable_presence` | counter | `{stanza}` | none | `waddle_s2s_route_unusable_presence_total` | Presence from S2S streams dropped unanswered because an address it carries is outside the identity profile or it is not valid presence. |
| `waddle.s2s.route.room_traffic.waits` | counter | `{stanza}` | none | `waddle_s2s_route_room_traffic_waits_total` | Stanzas from S2S streams to local full JIDs that found the room delivery queues full (the room's, a worker's, or the remote domain's share of a worker's), so that their stream stopped reading until they fit. |
| `waddle.s2s.route.room_traffic.overloaded` | counter | `{stanza}` | none | `waddle_s2s_route_room_traffic_overloaded_total` | Stanzas from S2S streams to local full JIDs that found no place on the room delivery queues within the wait's deadline; the stanza is lost and its stream closed with `resource-constraint`. |
| `waddle.s2s.route.room_traffic.delivery_timeouts` | counter | `{stanza}` | none | `waddle_s2s_route_room_traffic_delivery_timeouts_total` | Deliveries of remote room traffic to a local occupant abandoned after the delivery timeout. |
| `waddle.s2s.route.room_traffic.store_failures` | counter | `{stanza}` | none | `waddle_s2s_route_room_traffic_store_failures_total` | Stanzas from S2S streams to local full JIDs dropped unanswered because the remote room occupancy store failed while deciding whether they are room traffic. |
| `waddle.s2s.route.room_requests.waits` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_waits_total` | Stanzas from S2S streams to the MUC service that found the room request queues or their domain's request rate exhausted, so that their stream stopped reading until they fit. |
| `waddle.s2s.route.room_requests.overloaded` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_overloaded_total` | Stanzas from S2S streams to the MUC service that found no place on the room request queues within the wait's deadline; the stanza is lost and its stream closed with `resource-constraint`. |
| `waddle.s2s.route.room_requests.timeouts` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_timeouts_total` | Requests of remote principals to the MUC service abandoned after the room host's deadline, the placement of a request for a room another replica holds included. |
| `waddle.s2s.route.room_requests.carried` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_carried_total` | Stanzas from S2S streams for rooms another replica holds that were carried to that replica with their sender's stream evidence. Counted on the replica the stream landed on. |
| `waddle.s2s.route.room_requests.not_carried` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_not_carried_total` | Stanzas from S2S streams for rooms another replica holds that could not be carried there (its queues were full, it no longer held the room, it could not be reached or it predates the relay message), so that their sender was answered with a retryable error. A sustained rate during a rolling update is expected; outside one it means replicas cannot reach each other's relay. |
| `waddle.s2s.route.room_requests.account_limited` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_account_limited_total` | Stanzas from S2S streams to the MUC service not taken because their account had spent a share of its domain's request rate; each was answered with `resource-constraint` or dropped, and its stream went on. |
| `waddle.s2s.route.room_requests.joins_dropped` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_joins_dropped_total` | Joins from S2S streams to the MUC service dropped unanswered because their account had spent its share of presence and its allowance of answered refusals. A sustained rate means a remote client joins faster than its share allows. |
| `waddle.s2s.route.room_requests.exempt_dropped` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_exempt_dropped_total` | Leaves, errors and ignored presence from S2S streams to the MUC service dropped because their account had spent its allowance of such stanzas. |
| `waddle.s2s.route.room_requests.room_full` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_room_full_total` | Stanzas from S2S streams to the MUC service not taken because their room's share of the room request queues was full; each was answered within its domain's allowance of answered refusals, or dropped. |
| `waddle.s2s.route.room_requests.listings_refused` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_listings_refused_total` | Requests of remote principals for the MUC service's room list refused because the replica was building as many as it builds at once (four), or as many for the asker's domain as one domain may hold (two). Each was answered with `resource-constraint`. |
| `waddle.s2s.route.room_requests.leaves_kept` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_leaves_kept_total` | Leaves and errors from S2S streams for rooms another replica holds that could not be placed, kept to be placed again at the next sweeps. |
| `waddle.s2s.route.room_requests.leaves_dropped` | counter | `{stanza}` | none | `waddle_s2s_route_room_requests_leaves_dropped_total` | Leaves and errors from S2S streams for rooms another replica holds dropped because they could not be placed, nor kept to be placed again, in time. Their occupants stay seated until their seat ends otherwise. |
| `waddle.s2s.route.seat_traffic.refused` | counter | `{stanza}` | none | `waddle_s2s_route_seat_traffic_refused_total` | Stanzas of local rooms for remote JIDs refused when they were to be sent, because the remote domain is not admitted, the JID holds no seat or the stanza is misaddressed. |
| `waddle.s2s.route.seats.ended_undeliverable` | counter | `{seat}` | none | `waddle_s2s_route_seats_ended_undeliverable_total` | Seats of remote occupants ended because a stanza of their room found no place for the remote domain or could not be handed to its stream. |
| `waddle.s2s.route.seats.ended_stream_lost` | counter | `{seat}` | none | `waddle_s2s_route_seats_ended_stream_lost_total` | Seats of remote occupants ended because a stream between their domain and the MUC service failed. |
| `waddle.s2s.route.seats.kept_pair_carried` | counter | `{stream}` | none | `waddle_s2s_route_seats_kept_pair_carried_total` | Failed inbound streams whose domain pair another stream still carried, so the seats of the pair's remote occupants were kept. |
| `waddle.s2s.route.seats.ended_room_closed` | counter | `{seat}` | none | `waddle_s2s_route_seats_ended_room_closed_total` | Seats of remote occupants ended because their room's `federated` setting was turned off. |
| `waddle.s2s.rooms.archive_writes.limited` | counter | `{stanza}` | none | `waddle_s2s_rooms_archive_writes_limited_total` | Messages of remote occupants refused with `resource-constraint` because their account had spent its allowance of archived room messages (five a second, thirty at once), or because the allowance table was full of accounts still spending theirs. Emitted by the room host (`server/crates/waddle-server/src/server/routes/websocket/s2s_rooms.rs`). |
| `waddle.s2s.route.account_traffic.timeouts` | counter | `{stanza}` | none | `waddle_s2s_route_account_traffic_timeouts_total` | Stanzas from S2S streams to local accounts abandoned because the account side did not decide them before the deadline. |
| `waddle.s2s.route.account_traffic.replies_dropped` | counter | `{stanza}` | none | `waddle_s2s_route_account_traffic_replies_dropped_total` | Replies to remote senders the account side produced beyond the bound for one received stanza, dropped unsent. |
| `waddle.s2s.route.account_requests.refused` | counter | `{stanza}` | none | `waddle_s2s_route_account_requests_refused_total` | IQ requests from S2S streams to local resources refused with `resource-constraint` because too many were pending from their domain or on the replica. |
| `waddle.s2s.route.account_traffic.share_full` | counter | `{stanza}` | none | `waddle_s2s_route_account_traffic_share_full_total` | Messages and presence of local accounts for remote JIDs refused because the account's share of that pair's queue was full. |
| `waddle.s2s.account_traffic.not_admitted` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_not_admitted_total` | Stanzas from S2S streams that reached the account side from a domain the replica's S2S snapshot no longer admits (ADR-0020 D1), applied and answered with nothing. |
| `waddle.s2s.account_traffic.pending_dropped` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_pending_dropped_total` | Subscription requests from remote senders dropped unanswered because the account's record of pending requests, or its share for the sender's domain, was full, or the request was too large to record. |
| `waddle.s2s.account_traffic.direct_accepted` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_direct_accepted_total` | Direct messages from remote senders accepted through the ingress authority for a local account: archived, projected, delivered or stored as a local direct message is for its recipient. |
| `waddle.s2s.account_traffic.direct_refused` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_direct_refused_total` | Direct messages from remote senders refused unanswered by a rule before or at their ingress acceptance: a plan that would answer the sender or act beyond the recipient's account, a retraction or correction of another sender's message, a message that could not be read as offered, or an acceptance whose assertions refused the sender's domain or the recipient. |
| `waddle.s2s.account_traffic.direct_failed` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_direct_failed_total` | Direct messages from remote senders whose ingress acceptance did not commit (a timeout, an exhausted retry, an unknown outcome, a storage failure), lost unanswered: S2S has no retransmission of a stanza the receiving server took. |
| `waddle.s2s.account_traffic.offline_stored` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_offline_stored_total` | Messages from remote senders without a resource that no resource of the local account took, stored whole in the account's offline store; a sender with a resource's message is accepted through the ingress authority instead (`direct_accepted`). |
| `waddle.s2s.account_traffic.offline_dropped` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_offline_dropped_total` | Messages from remote senders that a detached session or the offline store would have held, dropped unanswered because a bound on offline storage for remote senders, or the offline store's quota for a message stored whole, was reached. |
| `waddle.s2s.account_traffic.offline_unadmitted` | counter | `{stanza}` | none | `waddle_s2s_account_traffic_offline_unadmitted_total` | Stored messages from remote senders deleted before an offline flush because the replica's S2S snapshot no longer admits the sender's domain (ADR-0020 D1). |
| `waddle.s2s.spaces.requests.limited` | counter | `{stanza}` | none | `waddle_s2s_spaces_requests_limited_total` | Requests from S2S streams for the Spaces PubSub service that were not taken because their account's share (ten a second, forty at once) or their domain's allowance (fifty a second, two hundred at once) was spent; answered with `resource-constraint` within the domain's allowance of such answers and dropped beyond it. |
| `waddle.s2s.spaces.requests.timed_out` | counter | `{stanza}` | none | `waddle_s2s_spaces_requests_timed_out_total` | Requests from S2S streams for the Spaces PubSub service that the Spaces host did not decide within the router's bound (5 s), so that no answer was sent. |
| `waddle.s2s.spaces.requests.not_admitted` | counter | `{stanza}` | none | `waddle_s2s_spaces_requests_not_admitted_total` | Requests that reached the Spaces host from a domain the replica's S2S snapshot does not admit, or over a stream below what host policy requires (ADR-0020 D1), refused with `forbidden`. Emitted by the Spaces host (`server/crates/waddle-server/src/server/routes/websocket/s2s_spaces.rs`), as are the four counters after the next two. |
| `waddle.s2s.spaces.events.queued` | counter | `{stanza}` | none | `waddle_s2s_spaces_events_queued_total` | Event notifications of local Spaces queued for subscribers at other servers, one for each subscribed account. |
| `waddle.s2s.spaces.events.dropped` | counter | `{stanza}` | none | `waddle_s2s_spaces_events_dropped_total` | Event notifications of local Spaces for subscribers at other servers dropped by the router: the subscriber's domain is not admitted now, at the hand-over or at the write, its domain pair could open no queue, or the pair held its share of queued notifications (256). |
| `waddle.s2s.spaces.publications.dropped` | counter | `{event}` | none | `waddle_s2s_spaces_publications_dropped_total` | Publications of local Spaces whose event notifications were not sent to subscribers at other servers because the replica's queue of publications awaiting that decision (1,024) was full. The decision is taken off the publisher's request; local subscribers are notified all the same. |
| `waddle.s2s.spaces.publications.discarded` | counter | `{event}` | none | `waddle_s2s_spaces_publications_discarded_total` | Publications of local Spaces queued for subscribers at other servers that were never decided because the replica's state or its Spaces host was shutting down. Local subscribers are notified all the same. |
| `waddle.s2s.spaces.events.limited` | counter | `{event}` | none | `waddle_s2s_spaces_events_limited_total` | Publications of local Spaces not queued for subscribers at other servers because the Space had spent its allowance of notified publications (four a second, sixteen at once, for each replica). The allowance is taken as a publication is handed over, before anything is read, so this also counts busy Spaces that have no subscriber at another server. Local subscribers are notified all the same. |
| `waddle.s2s.spaces.events.withheld` | counter | `{stanza}` | none | `waddle_s2s_spaces_events_withheld_total` | Event notifications of local Spaces withheld from one subscriber at another server: its subscription is no longer deliverable, its standing no longer lets it read the Space, it may not be told of the room the event names, or the router dropped the notification. |
| `waddle.s2s.spaces.subscriptions.limited` | counter | `{stanza}` | none | `waddle_s2s_spaces_subscriptions_limited_total` | Subscriptions of remote accounts to local Spaces refused with `resource-constraint` because a bound was reached: 256 remote subscribers for one Space, 32 of one remote domain in one Space, 64 subscriptions for one remote account, 4,096 for one remote domain or 65,536 in the deployment. |

## All other metrics

| Metric | Type | Unit | Attributes | Derived Prometheus name | Meaning |
|---|---|---|---|---|---|
| `http.server.request.duration` | histogram | `s` | `route`, `status_class` | `http_server_request_duration_seconds_bucket / _sum / _count` | HTTP server request duration. |
| `ingress.alias.outcomes` | counter | `{event}` | `outcome` | `ingress_alias_outcomes_total` | Ingress authority alias.outcomes. |
| `ingress.decisions` | counter | `{event}` | `class` | `ingress_decisions_total` | Ingress authority decisions. |
| `ingress.effects.unresolved` | counter | `{event}` | `kind`, `phase` | `ingress_effects_unresolved_total` | Ingress authority effects.unresolved. |
| `ingress.gc.reclaimed.orphan.proofs` | counter | `{proof}` | — | `ingress_gc_reclaimed_orphan_proofs_total` | Orphan stream-management append proofs reclaimed by ingress retention GC runs (lower bound: progress lost to external cancellation is not counted). |
| `ingress.gc.reclaimed_messages` | counter | `{message}` | — | `ingress_gc_reclaimed_messages_total` | Ingress messages reclaimed by ingress retention GC runs (lower bound: progress lost to external cancellation is not counted). |
| `ingress.gc.runs` | counter | `{run}` | `outcome` | `ingress_gc_runs_total` | Ingress retention GC runs by closed outcome. |
| `ingress.mailbox.deliveries` | counter | `{event}` | `outcome` | `ingress_mailbox_deliveries_total` | Mailbox poller attempts at a lane head, by how they ended (`decided`, `retried`, `dead_lettered`, `exhausted_rejected`). |
| `ingress.mailbox.hints` | counter | `{event}` | `outcome` | `ingress_mailbox_hints_total` | Mailbox wake-up hints sent to a peer, by whether it acknowledged. |
| `ingress.mailbox.room_acceptances` | counter | `{event}` | `outcome` | `ingress_mailbox_room_acceptances_total` | Origin waits for a remote room owner's decision, by how they ended. |
| `ingress.mailbox.room_requests.unconsumed_decisions` | gauge | `{request}` | — | `ingress_mailbox_room_requests_unconsumed_decisions` | Decided room requests no origin has consumed after the consumption grace. |
| `ingress.maintenance.departed_occupant_copies` | counter | `{copy}` | — | `ingress_maintenance_departed_occupant_copies_total` | Frozen MUC occupant copies settled by maintenance because the occupant is no longer in the room. |
| `ingress.maintenance.reclaimed_observer_rows` | counter | `{row}` | `table` | `ingress_maintenance_reclaimed_observer_rows_total` | Settled room-observer history rows reclaimed by bounded maintenance retention, by table. |
| `ingress.maintenance.recovered_obligations` | counter | `{obligation}` | — | `ingress_maintenance_recovered_obligations_total` | Ingress obligations recovered by bounded maintenance. |
| `ingress.maintenance.runs` | counter | `{run}` | `phase`, `outcome` | `ingress_maintenance_runs_total` | Ingress maintenance runs by phase and outcome. |
| `ingress.maintenance.terminalized_messages` | counter | `{message}` | — | `ingress_maintenance_terminalized_messages_total` | Ingress messages terminalized by bounded maintenance. |
| `ingress.maintenance.unrecoverable_obligations` | counter | `{obligation}` | `kind`, `reason` | `ingress_maintenance_unrecoverable_obligations_total` | Ingress obligations that maintenance cannot recover, by kind and reason. |
| `ingress.recovery.reachability_blocked` | counter | `{probe}` | `reason` | `ingress_recovery_reachability_blocked_total` | Recovery absence probes blocked by endpoint presence or unavailable evidence, by reason. |
| `ingress.tx.duration` | histogram | `s` | — | `ingress_tx_duration_seconds_bucket / _sum / _count` | Ingress authority transaction duration. |
| `ingress.tx.retries` | counter | `{transaction}` | — | `ingress_tx_retries_total` | Ingress authority transaction retries. |
| `muc.ghost_occupants.evicted` | counter | `{occupant}` | — | `muc_ghost_occupants_evicted_total` | MUC occupants evicted because a stalled groupchat obligation proved their occupancy abandoned. |
| `waddle.account_retirement.detached_sessions_ended` | counter | `{session}` | — | `waddle_account_retirement_detached_sessions_ended_total` | Detached stream-management sessions of retired accounts this replica ended when it took up the request to end their sessions ([account retirement](account-retirement.md)). |
| `waddle.account_retirement.offline_messages_discarded` | counter | `{message}` | — | `waddle_account_retirement_offline_messages_discarded_total` | Messages of retired accounts' offline stores this replica removed after ending their sessions: what the end of a live stream moved there after the retirement emptied the store. |
| `waddle.account_retirement.promotion_discarded` | counter | `{stanza}` | — | `waddle_account_retirement_promotion_discarded_total` | Stanzas a XEP-0198 promotion discarded because their recipient's account was retired: what a retired account's ended session held, or what its ingress custody kept, that no resource and no offline store of the account takes ([account retirement](account-retirement.md)). |
| `waddle.account_retirement.requests_completed` | counter | `{request}` | — | `waddle_account_retirement_requests_completed_total` | Requests to end a retired account's sessions this replica completed: it left no stream, no detached session and no offline message of the account. |
| `waddle.account_retirement.streams_closed` | counter | `{stream}` | — | `waddle_account_retirement_streams_closed_total` | Live streams of retired accounts this replica closed with `<not-authorized/>`. |
| `waddle.actor.message.handle.duration` | histogram | `s` | `actor` | `waddle_actor_message_handle_duration_seconds_bucket / _sum / _count` | Time a message handler occupied its actor, excluding work handed off through a delegated reply. |
| `waddle.actor.message.queue_wait` | histogram | `s` | `actor` | `waddle_actor_message_queue_wait_seconds_bucket / _sum / _count` | Time from the sender building a message to its handler starting, for messages that reach a handler. |
| `waddle.actor.started` | counter | `{actor}` | `actor` | `waddle_actor_started_total` | Actors whose `on_start` succeeded and that began processing their mailbox. |
| `waddle.actor.stopped` | counter | `{actor}` | `actor`, `reason` | `waddle_actor_stopped_total` | Started actors that left their message loop, by actor kind and reason (`panicked` \| `stopped`). |
| `waddle.auth.failures` | counter | `1` | `stage`, `error_code` | `waddle_auth_failures_total` | Authentication rejections by stage and enumerated error code. |
| `waddle.auth.success` | counter | `1` | `stage` | `waddle_auth_success_total` | Successful authentication outcomes by stage. |
| `waddle.call.admin.call_failed` | counter | `1` | `op` | `waddle_call_admin_call_failed_total` | LiveKit admin control-plane calls that failed after final normalization, by operation. |
| `waddle.call.client.end` | counter | `1` | `platform`, `end_reason` | `waddle_call_client_end_total` | Client-reported call endings, counted only when media connected on the reporting device. |
| `waddle.call.client.kind` | counter | `1` | `kind`, `setup_outcome` | `waddle_call_client_kind_total` | Every permitted client lifecycle report, by call kind and the setup outcome the client reports. |
| `waddle.call.client.setup` | counter | `1` | `platform`, `setup_outcome` | `waddle_call_client_setup_total` | Every permitted client lifecycle report, by platform and the setup outcome the client reports. |
| `waddle.call.control.rate_limited` | counter | `1` | `surface` | `waddle_call_control_rate_limited_total` | Call-control requests rejected by local rate limits. |
| `waddle.call.reconcile.occupancy_failures` | counter | `1` | — | `waddle_call_reconcile_occupancy_failures_total` | LiveKit room occupancy probes failed during call reconciliation. |
| `waddle.call.reconcile.pass_duration` | histogram | `s` | — | `waddle_call_reconcile_pass_duration_seconds_bucket / _sum / _count` | Wall-clock duration of one LiveKit call reconciliation pass. |
| `waddle.call.reconcile.rooms_adopted` | counter | `1` | — | `waddle_call_reconcile_rooms_adopted_total` | LiveKit rooms adopted into the local call registry. |
| `waddle.call.reconcile.rooms_examined` | counter | `1` | — | `waddle_call_reconcile_rooms_examined_total` | LiveKit rooms examined by call reconciliation. |
| `waddle.call.reconcile.rooms_swept` | counter | `1` | — | `waddle_call_reconcile_rooms_swept_total` | LiveKit rooms with registry ghosts swept by reconciliation. |
| `waddle.call.setup.attempted` | counter | `1` | — | `waddle_call_setup_attempted_total` | Jingle call setups attempted (session-initiate). |
| `waddle.call.setup.failed` | counter | `1` | `reason` | `waddle_call_setup_failed_total` | Jingle call setups that failed, by reason. |
| `waddle.call.setup.ok` | counter | `1` | — | `waddle_call_setup_ok_total` | Jingle call setups that completed successfully. |
| `waddle.call.sfu_token.denied` | counter | `1` | `reason` | `waddle_call_sfu_token_denied_total` | SFU token requests denied by reason. |
| `waddle.call.sfu_token.minted` | counter | `1` | — | `waddle_call_sfu_token_minted_total` | LiveKit SFU tokens minted. |
| `waddle.call.signaling` | counter | `1` | `event` | `waddle_call_signaling_total` | JMI and Muji call-signaling events. |
| `waddle.call.teardown.outbox.depth` | histogram | `1` | — | `waddle_call_teardown_outbox_depth_bucket / _sum / _count` | Queued durable call teardown intents observed at sweep time. |
| `waddle.call.teardown.outbox.drained` | counter | `1` | — | `waddle_call_teardown_outbox_drained_total` | Durable call teardown intents completed by the outbox drain. |
| `waddle.call.teardown.outbox.failed` | counter | `1` | — | `waddle_call_teardown_outbox_failed_total` | Durable call teardown intents moved to terminal failure. |
| `waddle.call.teardown.outbox.oldest_age` | histogram | `s` | — | `waddle_call_teardown_outbox_oldest_age_seconds_bucket / _sum / _count` | Age of the oldest queued durable call teardown intent. |
| `waddle.call.teardown.outbox.requeued` | counter | `1` | — | `waddle_call_teardown_outbox_requeued_total` | Durable call teardown intents requeued after retryable failures. |
| `waddle.call.teardown.stale_dropped` | counter | `1` | — | `waddle_call_teardown_stale_dropped_total` | Webhook-driven call teardowns dropped due to stale room or participant SIDs. |
| `waddle.call.turn_credentials` | counter | `1` | `outcome` | `waddle_call_turn_credentials_total` | TURN credential issuance attempts by outcome. |
| `waddle.call.webhook.events` | counter | `1` | `event`, `outcome` | `waddle_call_webhook_events_total` | LiveKit webhook ingestion events by event type and outcome. |
| `waddle.clustering.claim.cancellations` | counter | `{mutation}` | `operation` | `waddle_clustering_claim_cancellations_total` | Ownership claim mutations whose in-flight future was dropped (caller deadline or lifecycle cancellation) before the store answered. |
| `waddle.clustering.claim.mutations` | counter | `{mutation}` | `operation`, `result` | `waddle_clustering_claim_mutations_total` | External ownership claim mutation attempts by operation and bounded result. |
| `waddle.clustering.claims_abandoned_on_drain` | counter | `{claim}` | — | `waddle_clustering_claims_abandoned_on_drain_total` | Claims left held (not released) when a graceful drain's budget overran or a per-entity seal/release failed -- alert on any nonzero value |
| `waddle.clustering.fence.results` | counter | `{check}` | `result` | `waddle_clustering_fence_results_total` | Advisory ownership fence checks by bounded result. |
| `waddle.clustering.ingress_append.authorization_failed` | counter | `{obligation}` | `reason` | `waddle_clustering_ingress_append_authorization_failed_total` | Relayed ingress append authority failures -- indeterminate means this node could not read canonical state. |
| `waddle.clustering.remote_resource_updates` | counter | `{update}` | `outcome` | `waddle_clustering_remote_resource_updates_total` | Sender-interpreted outcomes of remote-resource state updates |
| `waddle.db.health_check.failed` | counter | `1` | — | `waddle_db_health_check_failed_total` | Database health-check failures (failed probe query, unavailable connection, or unreachable DB actor), across the readiness and detailed-health paths. |
| `waddle.extension.observation.invocation.duration` | histogram | `ms` | — | `waddle_extension_observation_invocation_duration_milliseconds_bucket / _sum / _count` | Wall time of a room extension invocation including its provider request. |
| `waddle.extension.observation.publication.delay` | histogram | `ms` | — | `waddle_extension_observation_publication_delay_milliseconds_bucket / _sum / _count` | Time from committed room source to durable room result publication. |
| `waddle.extension.observation.published` | counter | `1` | — | `waddle_extension_observation_published_total` | Saved room extension results admitted to the durable room broadcast path. |
| `waddle.extension.observation.queue.delay` | histogram | `ms` | — | `waddle_extension_observation_queue_delay_milliseconds_bucket / _sum / _count` | Time from committed room source to its first extension invocation. |
| `waddle.host_grant.enrolled_set.age` | gauge | `s` | — | `waddle_host_grant_enrolled_set_age_seconds` | Time since the load that installed this replica's host-grant enrolled set started, recorded on every refresh tick that completes; a rate-limited warning is logged once it exceeds 30 s. Readiness does not depend on it. |
| `waddle.host_grant.enrolled_set.load_started` | gauge | `s` | — | `waddle_host_grant_enrolled_set_load_started_seconds` | Unix time at which the load that installed this replica's host-grant enrolled set started, recorded by each successful load. Alert per replica on `time()` minus it above 30 s, or on the series being absent; under the default cumulative temporality it keeps being exported while refreshes fail or hang. Enrollment relies on a new scope only once every serving replica reports a value later than the enrollment commit. |
| `waddle.host_grant.fence.clock_skew` | gauge | `ms` | — | `waddle_host_grant_fence_clock_skew_milliseconds` | Difference between this replica's wall clock and PostgreSQL's `clock_timestamp()`, estimated from the middle of the round trip at the replica's last host-grant revocation fence registration or renewal. The fence's leases end on PostgreSQL's clock, so its hosts are kept in sync by NTP with an offset well under 1 s; a rate-limited warning is logged when the difference exceeds 500 ms by more than half the round trip. Alert per replica on a value above 500 ms that persists. |
| `waddle.host_grant.fence.lease_lapsed` | counter | `1` | — | `waddle_host_grant_fence_lease_lapsed_total` | Times this replica's host-grant revocation fence lease ran out before it was renewed. Rooms of enrolled scopes are refused on the replica until it registers again; alert on a rate above zero that persists. |
| `waddle.host_grant.fence.occupants_removed` | counter | `{session}` | — | `waddle_host_grant_fence_occupants_removed_total` | Occupant sessions the host-grant revocation sweep removed from managed rooms because the room's grants no longer admit their account. |
| `waddle.host_grant.increases_dropped` | counter | `{increase}` | — | `waddle_host_grant_increases_dropped_total` | Recorded host-grant increases a reconciliation pass dropped before activation because the S2S snapshot of its replica no longer admits their remote initiator's home or no longer lists it as a trusted home. Each drop is an operation of the scope; a value above zero after a change of the trusted-home list or the admission policy is expected. |
| `waddle.ingress.gc.orphan_sweep_failed` | counter | `{sweep}` | — | `waddle_ingress_gc_orphan_sweep_failed_total` | Orphan append-proof sweeps that failed without failing their maintenance pass. |
| `waddle.janitor.sweeps` | counter | `{sweep}` | `janitor`, `outcome` | `waddle_janitor_sweeps_total` | Periodic janitor sweep ticks by janitor and outcome. |
| `waddle.messages.delivered` | counter | `{message}` | `kind` | `waddle_messages_delivered_total` | Message stanzas with a body queued to a local recipient's connection, by kind (dm \| muc \| muc_pm). |
| `waddle.muc.admission.denied` | counter | `1` | `condition`, `deny_reason` | `waddle_muc_admission_denied_total` | MUC join admission denials by stanza error condition and refusal site. |
| `waddle.muc.mention_audience.check_failures` | counter | `{failure}` | `check` | `waddle_muc_mention_audience_check_failures_total` | Mentioned non-members excluded from a groupchat inbox fan-out because a mention-audience check failed, by check (account \| blocking \| room_access). |
| `waddle.muc.room_listings.remote_builds` | counter | `{build}` | — | `waddle_muc_room_listings_remote_builds_total` | Reads of the channel catalog and the live rooms that the MUC service's room listings for accounts at other servers were built from. A replica runs one at a time and answers remote askers from it for 2 s after it started, so the rate stays near one every 2 s however many listings are asked. |
| `waddle.muc.room_listings.session_builds` | counter | `{build}` | — | `waddle_muc_room_listings_session_builds_total` | Reads of the channel catalog and the live rooms that the MUC service's room listings for sessions were built from. A replica runs one at a time, and every session listing that arrived before it started is answered from it. |
| `waddle.push.pipeline` | counter | `{notification}` | `stage` | `waddle_push_pipeline_total` | Push notification pipeline transitions. |
| `waddle.push.provider` | counter | `{notification}` | `stage`, `provider` | `waddle_push_provider_total` | Push notification outcomes at the downstream provider boundary. |
| `waddle.room_effect_outbox.dead_lettered` | counter | `1` | `janitor` | `waddle_room_effect_outbox_dead_lettered_total` | Room-effect outbox rows dead-lettered after remaining globally unowned for 24h. |
| `waddle.room_effect_outbox.depth` | histogram | `1` | `janitor` | `waddle_room_effect_outbox_depth_bucket / _sum / _count` | Rows currently retained by the room-effect outbox at sweep time. |
| `waddle.room_effect_outbox.drained` | counter | `1` | `janitor` | `waddle_room_effect_outbox_drained_total` | Completed room-effect outbox deliveries. |
| `waddle.room_effect_outbox.requeued` | counter | `1` | `janitor` | `waddle_room_effect_outbox_requeued_total` | Room-effect outbox rows released for retry during a sweep. |
| `waddle.room_effect_outbox.stale` | counter | `1` | `janitor` | `waddle_room_effect_outbox_stale_total` | Room-effect outbox rows discarded as stale during a sweep. |
| `waddle.session.init.failed` | counter | `1` | `reason` | `waddle_session_init_failed_total` | Post-auth session initialization failures by reason; each one sent a client a stream-level internal-server-error. |
| `xmpp.muc.broadcast_build.duration` | histogram | `ms` | — | `xmpp_muc_broadcast_build_duration_milliseconds_bucket / _sum / _count` | MUC broadcast-build duration: groupchat broadcast accepted until the per-recipient outbound set is built. The full delivery fan-out interval is `xmpp.muc.fanout.latency`. |
| `xmpp.muc.fanout.latency` | histogram | `ms` | — | `xmpp_muc_fanout_latency_milliseconds_bucket / _sum / _count` | MUC fanout latency: accepted groupchat broadcast until all per-recipient sends are enqueued. |
| `xmpp.push.candidate_coalesced` | counter | `{notification}` | — | `xmpp_push_candidate_coalesced_total` | Duplicate XEP-0357 notification candidates coalesced at insertion. |
| `xmpp.push.candidate_created` | counter | `{notification}` | — | `xmpp_push_candidate_created_total` | XEP-0357 notification candidates inserted into the durable pipeline. |
| `xmpp.push.disposition` | counter | `{transition}` | `disposition` | `xmpp_push_disposition_total` | Push scheduling and custody dispositions. |
| `xmpp.push.failure_class` | counter | `{notification}` | `failure_class` | `xmpp_push_failure_class_total` | Push failures by send certainty. |
| `xmpp.push.failure_reason` | counter | `{notification}` | `reason` | `xmpp_push_failure_reason_total` | Push failures by bounded cause. |
| `xmpp.push.outbox_dead_lettered` | counter | `{notification}` | — | `xmpp_push_outbox_dead_lettered_total` | XEP-0357 notification outbox jobs terminally dead-lettered. |
| `xmpp.push.outbox_published` | counter | `{notification}` | — | `xmpp_push_outbox_published_total` | XEP-0357 notification outbox jobs accepted by the Push Service. |
| `xmpp.push.outbox_retry_scheduled` | counter | `{notification}` | `reason` | `xmpp_push_outbox_retry_scheduled_total` | XEP-0357 notification outbox jobs scheduled for retry. |
| `xmpp.push.provider.near_capacity_nodes` | gauge | `{node}` | — | `xmpp_push_provider_near_capacity_nodes` | Push nodes retaining at least ninety percent of their provider row cap. |
| `xmpp.push.provider.retained_max_per_node` | gauge | `{job}` | — | `xmpp_push_provider_retained_max_per_node` | Largest retained provider queue on a single push node. |
| `xmpp.push.queue.parked` | gauge | `{job}` | `queue` | `xmpp_push_queue_parked` | Current parked push rows retaining uncertain custody. |
| `xmpp.push.queue.snapshot_at` | gauge | `s` | `queue` | `xmpp_push_queue_snapshot_at_seconds` | Unix time of the last successful push queue snapshot. |
| `xmpp.push.queue.unresolved_failed` | gauge | `{job}` | `queue` | `xmpp_push_queue_unresolved_failed` | Failed push rows with known pre-send outcome and unsettled ancestry. |
| `xmpp.push.registration.inactive_node` | gauge | `{registration}` | — | `xmpp_push_registration_inactive_node` | Enabled configured-local push registrations whose owned provider node is inactive. |
| `xmpp.push.registration.missing_node` | gauge | `{registration}` | — | `xmpp_push_registration_missing_node` | Enabled configured-local push registrations without an owned provider node. |
| `xmpp.push.registration.snapshot_at` | gauge | `s` | — | `xmpp_push_registration_snapshot_at_seconds` | Unix time of the last successful configured-local registration snapshot. |
| `xmpp.push.suppressed` | counter | `{notification}` | `reason` | `xmpp_push_suppressed_total` | XEP-0357 notification candidates suppressed by a bounded policy reason. |
| `xmpp.remote_owner_mirror.attempts` | counter | `{mirror}` | `outcome` | `xmpp_remote_owner_mirror_attempts_total` | Remote owner mirror reconciliation attempts by bounded outcome. |
| `xmpp.remote_owner_mirror.inventory` | histogram | `{mirror}` | — | `xmpp_remote_owner_mirror_inventory_bucket / _sum / _count` | Remote owner mirror inventory sampled after each reconciliation page. |
| `xmpp.remote_owner_mirror.oldest_pending_age` | histogram | `s` | — | `xmpp_remote_owner_mirror_oldest_pending_age_seconds_bucket / _sum / _count` | Age since local detection of the oldest owed mirror retirement; zero with no pending work. |
| `xmpp.remote_owner_mirror.pending` | histogram | `{mirror}` | — | `xmpp_remote_owner_mirror_pending_bucket / _sum / _count` | Known owed remote owner retirements sampled after each reconciliation page. |
| `xmpp.sm.acks` | counter | `{ack}` | `outcome` | `xmpp_sm_acks_total` | Client XEP-0198 `<a/>` acknowledgements received by outcome. |
| `xmpp.sm.detached.append.rejected` | counter | `{stanza}` | `reason` | `xmpp_sm_detached_append_rejected_total` | Fenced detached SM append attempts reporting an error, by bounded reason. |
| `xmpp.sm.handled_progress` | counter | `{stanza}` | — | `xmpp_sm_handled_progress_total` | XEP-0198 handled-count progress accepted from client acknowledgements. |
| `xmpp.sm.request.latency` | histogram | `ms` | — | `xmpp_sm_request_latency_milliseconds_bucket / _sum / _count` | Latency from the oldest outstanding XEP-0198 `<r/>` request to the `<a/>` that covers its requested outbound frontier. Later requests coalesce while one remains outstanding. |
| `xmpp.sm.resume.results` | counter | `{attempt}` | `outcome` | `xmpp_sm_resume_results_total` | Terminal XEP-0198 resume attempts by outcome. |
| `xmpp.sm.unacked_evicted` | counter | `{stanza}` | `path` | `xmpp_sm_unacked_evicted_total` | XEP-0198 unacked stanzas evicted from an attached replay window, by path. |

Declarations for the table above are in
`waddle-xmpp/src/telemetry/reliability.rs` unless listed below:

- `waddle-server/src/clustering/metrics.rs`: `waddle.clustering.claims_abandoned_on_drain`.
- `waddle-server/src/clustering/route_bridge/registration/socket.rs`:
  `waddle.clustering.remote_resource_updates`.
- `waddle-server/src/db/database.rs`: `waddle.db.health_check.failed`.
- `waddle-server/src/ingress_substrate/substrate.rs`: `waddle.ingress.gc.orphan_sweep_failed`.
- `waddle-server/src/ingress/append_authority/implementation.rs`:
  `waddle.clustering.ingress_append.authorization_failed`.
- `waddle-server/src/room_observation/telemetry.rs`:
  `waddle.extension.observation.invocation.duration`,
  `waddle.extension.observation.publication.delay`, `waddle.extension.observation.published`,
  `waddle.extension.observation.queue.delay`.
- `waddle-server/src/server/routes/auth_telemetry.rs`: `waddle.auth.failures`,
  `waddle.auth.success`.
- `waddle-server/src/server/routes/interpret/room_dispatch.rs`: `xmpp.muc.fanout.latency`.
- `waddle-server/src/server/routes/livekit_webhook.rs`: `waddle.call.webhook.events`.
- `waddle-server/src/server/routes/websocket/call_signaling_telemetry.rs`: `waddle.call.signaling`.
- `waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs`:
  `waddle.muc.admission.denied`.
- `waddle-server/src/server/routes/websocket/registration.rs`: `waddle.session.init.failed`.
- `waddle-server/src/server/session_janitors/implementation.rs`:
  `waddle.room_effect_outbox.dead_lettered`, `waddle.room_effect_outbox.depth`,
  `waddle.room_effect_outbox.drained`, `waddle.room_effect_outbox.requeued`,
  `waddle.room_effect_outbox.stale`.
- `waddle-server/src/server/trace.rs`: `http.server.request.duration`.
- `waddle-xmpp/src/muc/room_broadcast.rs`: `xmpp.muc.broadcast_build.duration`.
- `waddle-xmpp/src/ownership/observe.rs`: `waddle.clustering.claim.cancellations`,
  `waddle.clustering.claim.mutations`, `waddle.clustering.fence.results`.
- `waddle-xmpp/src/protocol/handlers/extdisco.rs`: `waddle.call.turn_credentials`.
- `waddle-xmpp/src/telemetry/actor.rs`: `waddle.actor.message.handle.duration`,
  `waddle.actor.message.queue_wait`, `waddle.actor.started`, `waddle.actor.stopped`.
- `waddle-xmpp/src/telemetry/call.rs`: `waddle.call.admin.call_failed`,
  `waddle.call.control.rate_limited`, `waddle.call.reconcile.occupancy_failures`,
  `waddle.call.reconcile.pass_duration`, `waddle.call.reconcile.rooms_adopted`,
  `waddle.call.reconcile.rooms_examined`, `waddle.call.reconcile.rooms_swept`,
  `waddle.call.setup.attempted`, `waddle.call.setup.failed`, `waddle.call.setup.ok`,
  `waddle.call.sfu_token.denied`, `waddle.call.sfu_token.minted`,
  `waddle.call.teardown.outbox.depth`, `waddle.call.teardown.outbox.drained`,
  `waddle.call.teardown.outbox.failed`, `waddle.call.teardown.outbox.oldest_age`,
  `waddle.call.teardown.outbox.requeued`, `waddle.call.teardown.stale_dropped`.
- `waddle-xmpp/src/telemetry/messages.rs`: `waddle.messages.delivered`,
  `waddle.muc.mention_audience.check_failures`.
- `waddle-xmpp/src/telemetry/push_pipeline.rs`: `waddle.push.pipeline`, `waddle.push.provider`.
- `waddle-xmpp/src/telemetry/remote_owner.rs`: `xmpp.remote_owner_mirror.attempts`,
  `xmpp.remote_owner_mirror.inventory`, `xmpp.remote_owner_mirror.oldest_pending_age`,
  `xmpp.remote_owner_mirror.pending`.

The `waddle.actor.*` instruments are recorded for every kameo actor through
one process-wide observer in `waddle-server/src/actor_telemetry.rs`. The
`actor` allowlist in `waddle-xmpp/src/telemetry/attributes/actor.rs` is
`room_registry`, `room`, `user_registry`, `user`, `permission`,
`account_coordinator`, `db`, `relay`, `connection` and `other`. The message
type is not a metric attribute; use the `actor.handle_message` span's
`message` attribute in Tempo.

Queue wait includes sender backpressure and, for messages buffered during
startup, `on_start`. It is recorded only for messages that reach a handler:
a stuck actor shows as a handled-message rate that stops, not a long wait.
The handler-duration histogram's `_count` gives handled-message throughput;
failing asks are not counted as errors here. Handler duration measures the
time the handler occupied the actor, excluding work handed off through a
delegated reply. The room registry delegates replies; its end-to-end request
latency is `xmpp.actor.mailbox.latency`. For `actor="connection"`, these
message timings cover only binding commands; stanza turns are measured by
`waddle.connection_actor.turn.duration`.

Starts are counted after `on_start` succeeds, and stops only after a started
actor leaves its message loop. `reason="panicked"` covers a lifecycle hook or
handler panic or fatal error; `stopped` covers other stops, including normal
completion, a kill or a linked actor's death. Starts minus stops is a live
actor estimate that resets per pod restart.

Attribute-free reliability counters declared through the table in
`waddle-xmpp/src/telemetry/reliability.rs`, all created at zero at startup:

| Metric | Type | Unit | Attributes | Derived Prometheus name | Meaning |
|---|---|---|---|---|---|
| `muc.cleanup.not_occupant` | counter | `{departure}` | — | `muc_cleanup_not_occupant_total` | Generation-scoped MUC cleanup attempts whose resource is no longer an occupant. |
| `muc.cleanup.superseded` | counter | `{departure}` | — | `muc_cleanup_superseded_total` | Generation-scoped MUC cleanup attempts superseded by another occupant generation. |
| `xmpp.broadcast.delivered` | counter | `{stanza}` | — | `xmpp_broadcast_delivered_total` | Non-blocking broadcast attempts enqueued to a recipient. |
| `xmpp.broadcast.dropped_closed` | counter | `{stanza}` | — | `xmpp_broadcast_dropped_closed_total` | Non-blocking broadcast attempts dropped because the recipient channel was closed. |
| `xmpp.broadcast.dropped_full` | counter | `{stanza}` | — | `xmpp_broadcast_dropped_full_total` | Non-blocking broadcast attempts dropped because the recipient channel was full. |
| `xmpp.broadcast.not_connected` | counter | `{stanza}` | — | `xmpp_broadcast_not_connected_total` | Non-blocking broadcast attempts with no connected recipient. |
| `xmpp.delivery.retry_exhausted_drop` | counter | `{stanza}` | — | `xmpp_delivery_retry_exhausted_drop_total` | Deliveries dropped after bounded full-channel retries were exhausted. |
| `xmpp.delivery.terminal_error_drop` | counter | `{stanza}` | — | `xmpp_delivery_terminal_error_drop_total` | Actor-path deliveries dropped after an enqueue-uncertain terminal error. |
| `xmpp.dnd.projection_read_errored` | counter | `{event}` | — | `xmpp_dnd_projection_read_errored_total` | DND projection reads that failed open to inactive. |
| `xmpp.pending.flush_batches` | counter | `{event}` | — | `xmpp_pending_flush_batches_total` | Pending-delivery batches drained by offline-message flushes. |
| `xmpp.pending.flush_rows_pushed` | counter | `{row}` | — | `xmpp_pending_flush_rows_pushed_total` | Pending-delivery rows pushed to recovering resources. |
| `xmpp.pending_delivery.aged_out` | counter | `{row}` | — | `xmpp_pending_delivery_aged_out_total` | Pending-delivery rows removed after exceeding the configured maximum age. |
| `xmpp.pending_delivery.archive_lookup_transient_failure` | counter | `{event}` | — | `xmpp_pending_delivery_archive_lookup_transient_failure_total` | Pending-delivery flushes aborted by a transient archive lookup failure. |
| `xmpp.pending_delivery.orphan_claims_released` | counter | `{claim}` | — | `xmpp_pending_delivery_orphan_claims_released_total` | Orphaned pending-delivery claims released by the claim janitor. |
| `xmpp.pending_delivery.quota_exceeded` | counter | `{message}` | — | `xmpp_pending_delivery_quota_exceeded_total` | Offline messages rejected because the recipient pending-delivery quota was full. |
| `xmpp.pending_delivery.unresolved_poison_pill` | counter | `{row}` | — | `xmpp_pending_delivery_unresolved_poison_pill_total` | Pending-delivery rows dropped because their archived payload could not be resolved. |
| `xmpp.resolver.affiliation_sync_capacity_drop` | counter | `{event}` | — | `xmpp_resolver_affiliation_sync_capacity_drop_total` | Resolver-affiliation synchronization jobs dropped at scheduler capacity. |
| `xmpp.sm.detached_unacked_evicted` | counter | `{stanza}` | — | `xmpp_sm_detached_unacked_evicted_total` | Unacked stanzas evicted from a detached XEP-0198 session replay window. |
| `xmpp.sm.drain_timeout` | counter | `{event}` | — | `xmpp_sm_drain_timeout_total` | Graceful-shutdown XEP-0198 drains that exceeded their deadline. |
| `xmpp.sm.promotion_blocklist_failed` | counter | `{session}` | — | `xmpp_sm_promotion_blocklist_failed_total` | XEP-0198 promotion sessions skipped because blocklist loading failed. |
| `xmpp.sm.promotion_dead_lettered` | counter | `{session}` | — | `xmpp_sm_promotion_dead_lettered_total` | XEP-0198 promotion sessions dead-lettered after exhausting retries. |
| `xmpp.sm.promotion_not_promotable` | counter | `{stanza}` | — | `xmpp_sm_promotion_not_promotable_total` | Unacked stanzas that were not XEP-0160 promotion candidates. |
| `xmpp.sm.promotion_storage_failed` | counter | `{stanza}` | — | `xmpp_sm_promotion_storage_failed_total` | Unacked stanzas whose XEP-0160 promotion storage write failed. |
| `xmpp.sm.resume_window_clamped` | counter | `{session}` | — | `xmpp_sm_resume_window_clamped_total` | XEP-0198 sessions whose requested resume window was clamped. |
| `xmpp.sm.send_window_pause_timeouts` | counter | `{event}` | — | `xmpp_sm_send_window_pause_timeouts_total` | XEP-0198 send-window pauses that exceeded their acknowledgement deadline. |
| `xmpp.sm.send_window_pauses` | counter | `{event}` | — | `xmpp_sm_send_window_pauses_total` | XEP-0198 wire-write pauses engaged at the send-window high watermark. |
| `xmpp.user_actor.reaped` | counter | `{session}` | — | `xmpp_user_actor_reaped_total` | Empty user actors removed by the periodic reaper. |

`xmpp.sm.promotion_dead_lettered` is zero-registered only; no production
increment yet. Its generated increment helper has no production caller.

## Instruments created outside the macros

These instruments use raw meter builders, including the local `gauge!`
helper in `waddle-server/src/server/state_inventory_metrics.rs`. Their
attributes are set at each call site. Process and state-inventory
instruments have no attributes. `xmpp.actor.dead_detected` counts registry
removals without respawning; it is not a restart count.

| Metric | Instrument | Unit | Meaning | Source |
|---|---|---|---|---|
| `waddle.clustering.allowlist_size` | i64_gauge | `{peer}` | Currently enrolled clustering peer-allowlist entries | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.bootstrap_dials` | u64_counter | `{dial}` | Total bootstrap dial attempts to headless-DNS peers | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.claims_released_on_drain` | u64_counter | `{claim}` | Claims successfully released as part of a graceful drain | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.connected_peers` | i64_gauge | `{peer}` | Current number of connected clustering swarm peers | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.drain_duration` | f64_histogram | `ms` | Wall-clock duration of a graceful per-entity claim drain, from mark_draining to the batched release_many call (or abandonment) completing | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.node_heartbeat_age` | f64_gauge | `ms` | Milliseconds since this node's last successful node-lease heartbeat | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.node_heartbeat_write_latency` | f64_histogram | `ms` | Node-lease heartbeat CAS statement latency (pool wait + execution) | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.ordered_relay_acks` | u64_counter | `{reply}` | Internal ordered-relay ACK replies | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.ordered_relay_nacks` | u64_counter | `{reply}` | Internal ordered-relay NACK replies | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.orphan_terminal_cleanup_failures` | u64_counter | `{failure}` | Terminal orphan claim cleanup attempts that did not complete | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.orphan_work_queue_backpressure` | u64_counter | `{item}` | Orphan-reaper work rejected because its bounded queue was full | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.orphan_work_queue_depth` | u64_gauge | `{item}` | Current deduplicated orphan-reaper work queue depth | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.orphan_worker_failures` | u64_counter | `{failure}` | Orphan-reaper worker tasks that exited unexpectedly | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.peers_revoked` | u64_counter | `{peer}` | Peers revoked by allowlist refresh (live connections closed) | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.relay_respawns` | u64_counter | `{respawn}` | Supervised relay-actor respawns with same-name re-registration | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.remote_codec_drops` | u64_counter | `{payload}` | Remote payloads rejected by the XML codec (NACKed, never silent) | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.room_orphan_pending_depth` | u64_gauge | `{claim}` | Reclaimed room epochs awaiting adoption or release | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.room_orphan_pending_oldest_age` | u64_gauge | `ms` | Age of the oldest pending reclaimed room epoch | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.room_orphan_reconciliations` | u64_counter | `{claim}` | Proactive RoomActor orphan-claim reconciliation outcomes | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.routing_table_size` | i64_gauge | `{peer}` | Observed kademlia routing-table peer count | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.sm_orphan_candidate_page_size` | u64_histogram | `{claim}` | Bounded SM orphan candidates returned per cursor page | `waddle-server/src/clustering/metrics.rs` |
| `waddle.clustering.sm_orphan_malformed_quarantined` | u64_counter | `{claim}` | Malformed stale SM claim rows quarantined by the bounded scan | `waddle-server/src/clustering/metrics.rs` |
| `waddle.muc.local_departure_pending` | i64_gauge | `{departure}` | Local MUC departures retained until their projection converges. | `waddle-server/src/metrics.rs` |
| `waddle.muc.local_departure_retry` | u64_counter | `` | Local MUC departure retry outcomes. | `waddle-server/src/metrics.rs` |
| `waddle.muc.occupancy.handler.duration` | f64_histogram | `s` | Seconds from occupancy message receipt to reply | `waddle-xmpp/src/metrics.rs` |
| `waddle.muc.projection.commit` | u64_counter | `` | Durable commits authorizing MUC projections | `waddle-xmpp/src/metrics.rs` |
| `waddle.muc.projection.commit.duration` | f64_histogram | `s` | Seconds spent committing a MUC projection revision | `waddle-xmpp/src/metrics.rs` |
| `waddle.muc.projection.refused` | u64_counter | `` | Rejected MUC projection authorizations | `waddle-xmpp/src/metrics.rs` |
| `waddle.process.cpu.time` | f64_observable_counter | `s` | Total user+system CPU seconds consumed by the server process, from /proc/self/stat. | `waddle-server/src/server/process_metrics.rs` |
| `waddle.process.open_file_descriptors` | i64_observable_gauge | `{fd}` | Open file descriptors held by the server process, from /proc/self/fd. | `waddle-server/src/server/process_metrics.rs` |
| `waddle.process.resident_memory` | i64_gauge | `By` | Server process resident set size, sampled from /proc/self/statm on each state-inventory publisher tick. Plot against waddle.state.* to attribute an RSS climb to the map driving it. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.process.start_time` | i64_observable_gauge | `s` | Unix time the server process started, captured at telemetry init. Deploys mint a new series (instance id is the pod name); in-place restarts keep the series and jump the value — detect restarts via time() - value < threshold, which covers both (#1435). | `waddle-server/src/server/process_metrics.rs` |
| `waddle.state.auth.device_auth` | i64_gauge | `{entry}` | Live DeviceAuthorization entries awaiting device-flow approval. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.auth.dynamic_oidc_client_locks` | i64_gauge | `{entry}` | Locks guarding dynamic OIDC client registration races. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.auth.dynamic_oidc_clients` | i64_gauge | `{entry}` | Cached dynamic OIDC client registrations. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.auth.pending_auth` | i64_gauge | `{entry}` | Live PendingAuthorization entries awaiting OAuth callback. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.auth.xmpp_auth_codes` | i64_gauge | `{entry}` | Live XmppAuthCode entries awaiting XMPP OAuth token exchange. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.caps.caps_cache` | i64_gauge | `{entry}` | Cached XEP-0115 disco#info bodies (LRU-bounded). | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.caps.pending_resolutions` | i64_gauge | `{entry}` | Outstanding XEP-0115 disco#info resolutions awaiting client reply. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.connections.full_jid_connections` | i64_gauge | `{entry}` | Live full-JID WebSocket connections registered with the server. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.connections.last_activity` | i64_gauge | `{entry}` | Bare JIDs with recorded XEP-0012 last-activity timestamps. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.connections.pending_subscription_stanzas` | i64_gauge | `{entry}` | Bare JIDs holding queued XEP-0162 subscription stanzas for offline peers. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.connections.presence_states` | i64_gauge | `{entry}` | Bare JIDs with tracked presence state. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.profile.avatar_source_locks` | i64_gauge | `{entry}` | Per-BareJid avatar-publish mutexes currently held or waiting. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.profile.provider_dispatch_in_flight` | i64_gauge | `{entry}` | Provider webhook dispatch tasks currently in flight. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.profile.publish_tracker_in_flight` | i64_gauge | `{entry}` | OIDC profile-publish background tasks currently in flight. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.rooms.dormant` | i64_gauge | `{entry}` | Rooms reported by IsDormant — reclaimable on the next dormancy janitor tick. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.rooms.total` | i64_gauge | `{entry}` | Total RoomActor instances tracked by RoomRegistryActor. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.sessions.resumable_sessions` | i64_gauge | `{entry}` | Durable detached XEP-0198 snapshots awaiting resume or expiry. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `waddle.state.sessions.sm_live_sessions` | i64_gauge | `{entry}` | Detached XEP-0198 sessions in the SM session registry. | `waddle-server/src/server/state_inventory_metrics.rs` |
| `xmpp.actor.mailbox.depth` | i64_gauge | `{message}` | Current actor mailbox depth | `waddle-xmpp/src/metrics.rs` |
| `xmpp.actor.mailbox.latency` | f64_histogram | `ms` | Actor mailbox request latency | `waddle-xmpp/src/metrics.rs` |
| `xmpp.actor.requests.dropped` | u64_counter | `{request}` | Actor requests dropped due to backpressure or actor shutdown | `waddle-xmpp/src/metrics.rs` |
| `xmpp.actor.requests.timeout` | u64_counter | `{request}` | Actor requests timed out waiting on mailbox or reply | `waddle-xmpp/src/metrics.rs` |
| `xmpp.actor.dead_detected` | u64_counter | `{actor}` | Actors a registry found dead and removed without respawning | `waddle-xmpp/src/metrics.rs` |
| `xmpp.auth.attempts` | u64_counter | `{attempt}` | Total authentication attempts | `waddle-xmpp/src/metrics.rs` |
| `xmpp.connections.active` | i64_observable_gauge | `{connection}` | Current number of active XMPP connections | `waddle-xmpp/src/metrics.rs` |
| `xmpp.extensions.embeds.added` | u64_counter | `{embed}` | Extension embed elements added to messages | `waddle-xmpp/src/metrics.rs` |
| `xmpp.extensions.enrichment.latency` | f64_histogram | `ms` | Runtime extension link enrichment latency | `waddle-xmpp/src/metrics.rs` |
| `xmpp.muc.messages` | u64_counter | `{message}` | Total MUC messages sent | `waddle-xmpp/src/metrics.rs` |
| `xmpp.muc.occupants` | i64_observable_gauge | `{user}` | Current number of MUC occupants | `waddle-xmpp/src/metrics.rs` |
| `xmpp.muc.presence` | u64_counter | `{event}` | Total MUC presence events (joins and leaves) | `waddle-xmpp/src/metrics.rs` |
| `xmpp.muc.rooms.active` | i64_observable_gauge | `{room}` | Current number of active MUC rooms | `waddle-xmpp/src/metrics.rs` |
| `xmpp.stanza.handler.timeout` | u64_counter | `{stanza}` | Inbound stanza handlers that exceeded the per-connection wedge backstop | `waddle-xmpp/src/metrics.rs` |
| `xmpp.stanza.latency` | f64_histogram | `ms` | XMPP stanza processing latency | `waddle-xmpp/src/metrics.rs` |
| `xmpp.stanzas.processed` | u64_counter | `{stanza}` | Total XMPP stanzas processed | `waddle-xmpp/src/metrics.rs` |
