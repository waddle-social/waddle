# #2046: `WsConnState` semantic inventory

Source review for `main@5763923e0cb15d3bbab9cf53800e5b1bdd41e183`, in the
PR #2300 worktree. This is a field and call-path review, not a claim that every
read, write, configuration, or transitive callee has been classified. The
[declaration](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1756)
has 46 non-`cfg(test)` fields and three `cfg(test)` fields. The live
[WebSocket connection](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L454)
constructs and retains this state. The compiled
[connection actor candidate](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L125)
can call the same frame handler, but that does not establish that `/ws` selects
it.

The refreshed compiler captures at `5763923e` have now been reconciled for
both default and clustering: each has 487 WS field sites, all 487 match their
historical counterparts by field, source file, mapped position, compiled
target, source expression and enclosing function. Four actor-core spans moved
three lines; no field sites were added or removed. The
[structured site reviews](ws-state-site-reviews.tsv) record 487 rows per profile
with current keys, direct operation and role, and source citations. Borrow
rows cite concrete downstream operations, which may be conditional. The
reviews do not establish that all indirect edges, shared-handle effects,
feature combinations, or runtime behavior were discovered.

The earlier [compiler-marker artifacts](README.md) were produced at
`3ae674c121c06b44934c915ecc0ef469dca54833`. Filtering their
[default](uses-default.tsv) and [clustering](uses-clustering.tsv) rows for
`WS.` gives 46 field tags and 487 direct field spans in each profile. The
larger 564-per-profile `WsConnState` family count in the README includes four
marked methods. These are profile-specific compiler references, not 974 unique
sites or classified operations. From that compiler base to `5763923e`, the
WebSocket changes are confined to `actor_local_endpoint.rs`,
`connection_actor_core.rs`, and `sasl.rs`; the latter moves a test module and
therefore shifts later source lines. This review uses current source links,
not those old coordinates. The refreshed captures were produced separately; this review did not run Cargo.
The historical artifact's cfg hints are not proof that an individual span is test code; its checked targets
were non-test libraries, as its README explains.

## Classes and operation groups

**A** = live authority for the stated decision, **I** = derived value or
resume carrier, **T** = transport, lifecycle or control plumbing, and **O** =
obsolete migration glue. Combined labels describe multiple roles; they do not
assign a future owner. An `&mut` handoff is listed as a potential mutation
boundary only when its downstream operation has been inspected. Constructor
initialization applies to every field; the table focuses on later operations.
No field is called O merely because other code keeps a copy. In particular,
removing a current mirror or an owner fence is not implied by its class.

| Fields | Class | Inspected operations and evidence |
| --- | --- | --- |
| `ingress_generation` | A/T | Minted at [construction](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1951); read into the connection-scoped [ingress submission](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1730). It is distinct from the binding and SM claim identities. |
| `connection_ref`, `binding_ref` | A/T | Physical identity is retained; bind/resume creates a distinct attempt binding and later [publishes it](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L640). SM resume clears an earlier attempt and [creates a fresh identity](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L985); [retirement takes it](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2027) and interrupts that binding. Output and force-detach check the exact binding. |
| `local_io_relinquishment`, `registry_owner`, `socket_lifecycle` | T; A; A/T | The socket [sets its relinquishment witness](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L482). Registration supplies it to the entry, sets the exact local `registry_owner`, and [publishes lifecycle](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L640). The owner is read for guarded writes/cleanup and cleared on [failed-resume reset](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L453); it is not a durable node or SM claim. |
| `phase`, `authenticated_session` | A | [Frame handling](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L774) changes phase or clears the session on protocol failures and passes both to SASL. OAuth/SCRAM [write authentication state](../../../server/crates/waddle-server/src/server/routes/websocket/sasl.rs#L241); SM resume can replace both. Bind, ingress, registration and cleanup read them. The machine's phase is separately synchronized as described below. |
| `stream_open_handled`, `stream_open_from`, `stream_open_wire_state` | A; A; T | Frame open [records the untrusted `from` hint](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L835); it is checked against authenticated identity, not itself identity proof. Helpers [mark semantic handling, pending/committed wire state and reset](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2123). Shutdown reads the committed state to decide whether a stream error is legal. |
| `occupancy_session` | A | Minted on construction, [adopted from a detached session on resume](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L1460), and reminted on failed-resume rollback. Registration, MUC/relay contexts and cleanup use it to fence a particular occupancy generation. |
| `sm_state`, `sm_ingress_fence`, `sm_inbound_completion` | A | Per-stream counters, replay state, exact ingress fence and ordered completion tracking. The frame dispatcher [reserves and settles](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1232) inbound work; SM control and output mutate state through further borrows. [Post-write SM enable](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2157) takes the staged commit and publishes the state and fence. Cleanup reads and mutates them while draining and promoting. |
| `inbound_frame_terminal`, `ordered_relay_handoff_tx` | A/T; T | The [socket installs the completion sender](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L492); frame dispatch borrows it for relay-origin construction; the non-clustering helper ignores it, while the clustering helper can clone it into a handoff. Revocation or failure [sets the terminal disposition](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1245); the connection loop consumes it and the handoff receiver completes or abandons reserved SM work. |
| `carbons_enabled`, `roster_interested`, `blocklist_interested` | A/I | IQ handlers set these socket fields through mutable borrows after owner-gated entry updates; see the detailed trace below. SM resume restores them, reset clears them, and registration [copies them to the new entry](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L654). They are live per-stream carriers as well as duplicated selection state, not proof of a single owner. |
| `presence_available`, `presence_show`, `presence_status`, `presence_priority`, `presence_payloads` | I | Restored on true SM resume, cleared on failed resume, then [republished on registration](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L399). Ordinary live presence updates the entry and registry rather than these socket fields. Terminal-recovery snapshot reads these carriers; it need not reflect later live presence. |
| `pending_subscribes_flushed` | I | Resume restores and reset clears this carrier; [registration reads it](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L714) to pre-claim the fresh entry's flush. Live available presence claims the owner-checked entry, and normal detach snapshots that entry's flag. The terminal-recovery snapshot reads the socket carrier instead. |
| `pending_resume_stream_id`, `pending_resume_h`, `pending_resume_claim` | A/T | [Resume stages](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L1470) the stream, acknowledged count and exact claim; [registration takes/finalizes](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L57) them. Failure and authority revocation clear or release them; dropping a guard is an operation, not merely a read. |
| `pending_finalized_resume_outcome` | T | Resume or registration stages an outcome; the loop [takes and records it only after a response frame was written](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2006). This is telemetry staging, not evidence that the peer received a frame merely because an outcome is present. |
| `recovered_pending_endpoint`, `resume_endpoint_handoff_blocked` | T | Failed-resume reset seals the exact entry, checks the paired receiver, then [retains an empty endpoint or blocks handback](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L352). Subsequent [registration takes the endpoint](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L632); this is bounded same-socket handback, not a general cutover. |
| `write_interruption`, `live_resume_control` | T | Constructed together; the live socket [clones them into the interruptible writer](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L482), and registration shares them with the entry. Clones are shared handles with their own interior state: moving the fields alone would not necessarily move all mutations or callers. |
| `suppress_sm_record_next_batch`, `pending_sm_enable_commit` | T; A/T | Resume [sets the one-shot suppression](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L1479), and the loop [reads and clears it](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1981) before choosing a replay policy. Enable [stages a guard-bearing commit](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L788); publication occurs only after the enable response write succeeds. |
| `deferred_inbound` | T | The batch writer retains non-ack frames while draining acknowledgements; the loop prioritizes this queue, and terminal cleanup [pops or discards](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2321) it. Its order is transport work, not independent durable acceptance. |
| `sm_recovery_required`, `terminal_sm_recovery` | A/T; A | [Beginning terminal recovery](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2033) sets the flag and initializes a separate queue from live SM metadata. Recording is capped; cleanup drains and [promotes instead of storing a resumable snapshot](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1013). |
| `pending_replay_completions`, `terminal_sm_recovery_dropped`, `terminal_sm_recovery_drop_warned` | A/T; T; T | Replay-only completions are [queued and settled](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2533) after cleanup's result. Recovery recording [increments the drop count at the cap](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2073) and logs once using the warning flag. |
| `send_window_pause_deadline`, `send_window_last_request_acked` | T | The [connection loop](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L560) arms/clears the pause deadline, tracks the last acknowledged count when requesting an ack, and applies backpressure while waiting. |
| `state_machine`, `keepalive_config` | A/T; T | Initialization [stores config and replaces the machine](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2179); bind/resume seeds a ready machine and blocklist, activity feeds it events, and `sync_state_machine_phase` mirrors closing. IQ blocking and message/replay paths also mutate it through borrows. Its blocklist is a session snapshot; persisted blocking storage remains separately authoritative. |

## Borrowed writes and callers

### IQ and presence

1. [`handle_xmpp_frame_impl`](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L672)
   destructures `&mut WsConnState` at line 687. For IQ it constructs
   [`IqConnState`](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1290)
   with mutable references to the three flags and `state_machine`, a shared
   `occupancy_session`, and an optional shared `registry_owner`. Its
   [`ordered_relay_origin`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/conn_state.rs#L20)
   is a separately computed value, not a `WsConnState` field. The dispatch is
   `handle_iq_with_conn_state` → namespace-specific handlers.
2. Carbons passes through [remote-target preflight](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/remote_destination.rs#L12)
   into [`handle_sans_io_iq`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/sans_io.rs#L170).
   Only after `set_carbons_enabled_if_owner` succeeds does it assign
   `*conn_state.carbons_enabled = enabled`, then attempt remote mirroring.
   This write is absent as a `WsConnState` receiver in compiler-marker spans.
3. Roster dispatch passes the same mutable flag into
   [`handle_roster_iq` → `handle_roster_get`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/roster/implementation.rs#L14).
   After a successful roster snapshot and owner-gated entry mark,
   [`*roster_interested = true`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/roster/implementation.rs#L104).
   Roster set does not take this path. Blocking's successful get similarly
   [marks the entry, writes `*blocklist_interested`, and mirrors](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/blocking.rs#L56).
   For block/unblock, successful storage updates are followed by a reload and
   [`state_machine.set_blocklist`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/blocking.rs#L193)
   when the machine and reload are available. These mutable borrows do not
   turn the persisted roster or blocklist into socket-owned storage.
4. The presence branch passes `phase`, session, occupancy and owner into a
   [presence context](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1311),
   not the five socket presence fields. Regular presence writes availability
   and full state through [owner-gated registry calls](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L54),
   and claims pending-subscribe delivery through the entry. Normal detach
   [reads those live entry/registry values](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L908);
   the separate [terminal snapshot](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2271)
   reads the socket carriers and is explicitly non-resumable. Remote mirror
   helpers run only with the `clustering` feature; their no-clustering
   counterparts are no-ops. The current owner-checked registry writes remain
   necessary even when mirror glue is ultimately replaced.

### Stream management, inbound completion and cleanup

1. The same destructuring builds [`SmCtx`](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L730),
   whose [declaration](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L331)
   exposes the concrete borrows. `handle_sm_stanza` dispatches enable,
   request, ack and resume. Request can flush completion state or close
   `phase`; [ack handling](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L525)
   mutates `sm_state` and can close `phase`, while consulting the fence.
2. Enable stages `pending_sm_enable_commit`; after the writer succeeds,
   `WsConnState::publish_pending_sm_enable` passes `&mut sm_state` and
   `&mut sm_ingress_fence` to [`SmEnableCommit::publish`](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L107).
   It assigns the fence, enables the stream and commits its guard when present; an
   owner-gated registry alias is a separate operation.
3. [`handle_sm_resume_terminal`](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L939)
   destructures `SmCtx` again. On a successful path it
   [restores/acknowledges SM and replaces](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L1426)
   the session, occupancy, three interests, five presence carriers,
   pending-subscribe carrier, pending resume triple, binding, phase and replay
   suppression. The outer [resume handler](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L914)
   stages the separate final outcome for write-gated observation. Failure and
   rollback are conditional; the list is not a claim that every attempt writes
   all fields.
4. For countable inbound stanzas, frame handling reserves an SM sequence,
   passes relay handoff context, and [completes or abandons](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1380)
   the borrowed completion tracker and `sm_state`. Relay completion later
   [finishes the same tracker](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2127).
   The message path separately lends the machine to
   [message handling](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/message.rs#L66)
   or [authoritative planning](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1693).
5. [Normal cleanup](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L828)
   checks exact owner and SM conditions, drains outbound work via mutable
   machine/SM borrows, and constructs a detached snapshot. The output path
   [records outbound SM entries](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L66);
   terminal recovery uses a separate capped buffer. [Failed resume reset](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L344)
   retires the exact binding, handles endpoint custody, clears/restores fields
   and rebuilds the pre-bind machine. This path must be considered with
   ordinary shutdown, not inferred solely from a field access at the call site.

## Configuration and evidence boundaries

- The table describes non-`cfg(test)` declarations. The separate fields
  `resume_binding_attempts`, `pre_final_principal_recheck_test_hook`, and
  `post_sm_finalization_test_hook` are explicitly
  [`cfg(test)`](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1765).
  `WsConnState::new` is test-only; `new_for_node_lease` is test or
  `test-support`, and the live constructor is
  [`new_for_connection_ref`](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1938).
  The IQ metadata-write wait hooks are also `cfg(test)`; they are not part of
  production sequencing.
- The two compiler profiles exclude unit-test targets and do not cover every
  feature combination or prove live execution. Individual reviewed paths above
  are non-test source unless a gate is named. The candidate actor core is
  compiled but not established as the selected `/ws` owner. Test fixtures and
  `test-support` methods must not be counted as live callers.
- Direct references in the TSV are compiler-resolved at their pinned source;
  the classifications here are manual judgments for the linked operations.
  They are not a site-by-site certification of either TSV, a full call graph,
  an exhaustive list of aliases, shared-handle mutations, external registry
  writers, or every `cfg` combination. In particular, machine/SM internals,
  all cleanup branches, all runtime routes, and actor-candidate integration
  require their own review before a deletion or ownership cutover.

## Exact compiler-span triage at the marker base

The following index accounts for **all 487 `WS.` field rows in each pinned
profile**. The two profiles have identical field-site keys after removing the
profile column; all are `waddle_server` library targets with `test=false`.
The old lexical `source_class_hint` marks 127 of these as `test`, but that
hint overextends field-level `cfg` attributes and is not target-level test
evidence. Each link label is `WebSocket-relative-file:marker-base-line:column`
and its destination is the corresponding line in the reviewed `5763923e`
source. Four spans in `connection_actor_core.rs` move down three lines;
unchanged source lines were mapped before linking. The columns identify
separate compiler spans even where two occur on one line.

Operations describe the **direct expression at the marked field**, not a
complete transitive effect or authority transfer. Source lines are linked for
inspection. They were triaged from the original compiler rows and the source
expression; they are not a substitute for a compiler R/W classification.

- **N**: constructor initialization (46 spans).
- **W**: direct field assignment (64 spans).
- **M**: direct mutating method or `take` on the field (51 spans).
- **R**: field value, shared borrow, or inspected read/accessor (262 spans).
- **B**: mutable borrow or destructure of `&mut WsConnState` (60 spans);
  this does not assert that the eventual callee writes it.
- **H**: operation on a shared handle with possible interior effects (4 spans).

The 60 B and four H sites are the exact review set for transitive effects.
Some important downstream writes are traced above; the listing does **not**
claim the effect of every borrow or every clone of a shared handle has been
proven. `R` records the local field access and likewise does not certify all
operations performed by an independently shared object. No site is counted
as O solely because it updates or reads an active mirror.

### `ingress_generation` — A/T
- **N**: [state.rs:1963:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1963).
- **R**: [frame.rs:1738:17](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1738).

### `connection_ref` — A/T
- **N**: [state.rs:1966:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1966).
- **R**: [actor_local_endpoint.rs:915:12](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L915), [batch_write.rs:350:35](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L350), [connection_actor_core.rs:100:10](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L100), [registration.rs:500:59](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L500), [registration.rs:508:51](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L508).
- **B**: [frame.rs:688:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L688).

### `local_io_relinquishment` — T
- **N**: [state.rs:1967:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1967).
- **W**: [connection.rs:491:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L491).
- **R**: [registration.rs:641:28](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L641).

### `binding_ref` — A/T
- **N**: [state.rs:1968:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1968).
- **W**: [registration.rs:640:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L640).
- **M**: [registration.rs:461:9](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L461), [state.rs:2028:32](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2028).
- **R**: [batch_write.rs:337:22](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L337), [connection.rs:681:57](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L681), [connection.rs:701:41](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L701), [connection.rs:721:41](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L721), [connection.rs:1083:13](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1083), [connection.rs:1164:17](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1164), [connection.rs:1174:27](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1174), [stream_management/registration.rs:359:26](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L359).
- **B**: [frame.rs:689:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L689).

### `phase` — A
- **N**: [state.rs:1971:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1971).
- **W**: [cleanup.rs:815:13](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L815), [cleanup.rs:2786:5](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2786), [frame.rs:402:5](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L402), [frame.rs:1788:13](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1788), [frame.rs:1817:13](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1817), [stream_management/registration.rs:476:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L476), [stream_management/registration.rs:535:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L535).
- **R**: [batch_write.rs:333:28](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L333), [batch_write.rs:750:24](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L750), [batch_write.rs:1134:24](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1134), [cleanup.rs:814:35](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L814), [cleanup.rs:839:28](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L839), [cleanup.rs:853:21](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L853), [cleanup.rs:890:22](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L890), [cleanup.rs:1741:65](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1741), [connection.rs:630:32](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L630), [connection.rs:641:32](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L641), [connection.rs:653:32](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L653), [connection.rs:680:46](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L680), [connection.rs:980:36](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L980), [connection.rs:1006:32](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1006), [connection.rs:1078:31](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1078), [connection.rs:1110:44](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1110), [connection.rs:1160:35](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1160), [connection.rs:1173:24](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1173), [connection.rs:1274:20](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1274), [connection.rs:1907:48](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1907), [connection.rs:2076:20](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2076), [connection.rs:2084:17](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2084), [connection.rs:2455:18](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2455), [connection_actor_core.rs:203:64](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L206), [frame.rs:681:42](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L681), [frame.rs:1693:22](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1693), [frame.rs:1788:51](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1788), [frame.rs:1810:72](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1810), [frame.rs:1817:51](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1817), [outbound.rs:70:28](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L70), [outbound.rs:220:62](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L220), [registration.rs:468:21](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L468), [registration.rs:845:30](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L845), [registration.rs:873:19](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L873), [state.rs:2161:25](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2161), [state.rs:2252:25](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2252).
- **B**: [batch_write.rs:727:30](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L727), [batch_write.rs:1103:30](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1103), [frame.rs:400:45](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L400), [frame.rs:656:14](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L656), [frame.rs:692:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L692).

### `stream_open_handled` — A
- **N**: [state.rs:1972:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1972).
- **W**: [state.rs:2124:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2124), [state.rs:2143:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2143).
- **R**: [state.rs:2149:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2149).

### `stream_open_from` — A
- **N**: [state.rs:1973:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1973).
- **W**: [state.rs:2144:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2144).
- **B**: [frame.rs:693:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L693).

### `stream_open_wire_state` — T
- **N**: [state.rs:1974:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1974).
- **W**: [state.rs:2125:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2125), [state.rs:2135:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2135), [state.rs:2145:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2145).
- **R**: [state.rs:2132:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2132), [state.rs:2150:25](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2150).

### `authenticated_session` — A
- **N**: [state.rs:1975:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1975).
- **W**: [frame.rs:401:5](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L401).
- **R**: [cleanup.rs:950:17](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L950), [cleanup.rs:961:23](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L961), [cleanup.rs:1037:33](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1037), [cleanup.rs:1238:25](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1238), [cleanup.rs:2280:18](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2280), [connection.rs:933:35](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L933), [connection_actor_core.rs:204:23](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L207), [frame.rs:1605:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1605), [outbound.rs:221:27](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L221), [replay.rs:254:29](../../../server/crates/waddle-server/src/server/routes/websocket/replay.rs#L254).
- **B**: [frame.rs:694:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L694).

### `occupancy_session` — A
- **N**: [state.rs:1976:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1976).
- **W**: [stream_management/registration.rs:475:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L475).
- **R**: [cleanup.rs:969:32](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L969), [cleanup.rs:1458:62](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1458), [cleanup.rs:1505:62](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1505), [cleanup.rs:1542:46](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1542), [cleanup.rs:1706:42](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1706), [cleanup.rs:2286:28](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2286), [cleanup.rs:2743:46](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2743), [registration.rs:550:46](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L550), [registration.rs:590:62](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L590), [registration.rs:680:64](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L680), [registration.rs:688:55](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L688), [registration.rs:849:13](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L849).
- **B**: [frame.rs:695:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L695).

### `sm_state` — A
- **N**: [state.rs:1977:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1977).
- **M**: [batch_write.rs:433:17](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L433), [batch_write.rs:575:13](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L575), [batch_write.rs:663:5](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L663), [batch_write.rs:771:25](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L771), [batch_write.rs:897:20](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L897), [batch_write.rs:1001:21](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1001), [cleanup.rs:2727:5](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2727), [connection.rs:607:21](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L607), [outbound.rs:79:37](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L79), [outbound.rs:90:21](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L90), [outbound.rs:211:17](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L211), [outbound.rs:289:21](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L289), [stream_management/registration.rs:143:17](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L143), [stream_management/registration.rs:155:17](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L155), [stream_management/registration.rs:492:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L492), [cleanup.rs:1013:37](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1013), [cleanup.rs:1804:30](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1804).
- **R**: [batch_write.rs:364:37](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L364), [batch_write.rs:672:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L672), [batch_write.rs:698:33](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L698), [batch_write.rs:700:31](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L700), [batch_write.rs:701:33](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L701), [batch_write.rs:757:25](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L757), [batch_write.rs:779:26](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L779), [batch_write.rs:789:50](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L789), [batch_write.rs:864:14](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L864), [batch_write.rs:871:38](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L871), [batch_write.rs:881:5](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L881), [batch_write.rs:907:20](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L907), [cleanup.rs:812:12](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L812), [cleanup.rs:842:66](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L842), [cleanup.rs:874:8](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L874), [cleanup.rs:876:12](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L876), [cleanup.rs:889:37](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L889), [cleanup.rs:905:70](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L905), [cleanup.rs:925:70](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L925), [cleanup.rs:996:34](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L996), [cleanup.rs:1429:82](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1429), [cleanup.rs:1570:62](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1570), [cleanup.rs:1806:69](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1806), [cleanup.rs:1825:65](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1825), [cleanup.rs:2611:65](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2611), [cleanup.rs:2662:34](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2662), [cleanup.rs:2702:13](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2702), [cleanup.rs:2718:69](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2718), [connection.rs:571:38](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L571), [connection.rs:587:36](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L587), [connection.rs:590:59](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L590), [connection.rs:1928:26](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1928), [connection.rs:2015:17](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2015), [connection.rs:2031:17](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2031), [connection.rs:2040:17](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2040), [connection.rs:2050:17](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2050), [connection.rs:2077:25](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2077), [connection.rs:2078:25](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2078), [connection.rs:2284:30](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2284), [connection.rs:2285:12](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2285), [connection.rs:2425:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2425), [connection.rs:2433:25](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2433), [connection_actor_core.rs:203:43](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L206), [frame.rs:1596:21](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1596), [frame.rs:1602:21](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1602), [outbound.rs:66:16](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L66), [outbound.rs:91:48](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L91), [outbound.rs:97:36](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L97), [outbound.rs:105:28](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L105), [outbound.rs:220:47](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L220), [outbound.rs:274:35](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L274), [outbound.rs:288:20](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L288), [registration.rs:377:13](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L377), [registration.rs:385:12](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L385), [registration.rs:390:17](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L390), [registration.rs:534:21](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L534), [registration.rs:569:17](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L569), [registration.rs:617:17](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L617), [registration.rs:820:17](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L820), [state.rs:2041:31](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2041), [state.rs:2049:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2049), [state.rs:2050:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2050), [state.rs:2100:21](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2100), [stream_management/registration.rs:103:48](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L103), [stream_management/registration.rs:141:21](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L141), [stream_management/registration.rs:183:33](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L183), [stream_management/registration.rs:211:37](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L211), [stream_management/registration.rs:282:17](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L282), [stream_management/registration.rs:330:19](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L330).
- **B**: [batch_write.rs:726:30](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L726), [batch_write.rs:1102:30](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1102), [cleanup.rs:939:22](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L939), [cleanup.rs:949:22](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L949), [cleanup.rs:1237:30](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1237), [connection.rs:2148:53](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2148), [connection.rs:2241:53](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2241), [frame.rs:657:14](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L657), [frame.rs:696:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L696), [frame.rs:1783:22](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1783), [frame.rs:1805:14](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1805), [state.rs:2164:18](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2164).

### `sm_ingress_fence` — A
- **N**: [state.rs:1978:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1978).
- **W**: [cleanup.rs:2726:5](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2726), [stream_management/registration.rs:491:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L491).
- **R**: [batch_write.rs:729:25](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L729), [batch_write.rs:1105:25](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1105), [cleanup.rs:1789:11](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1789), [frame.rs:1601:17](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1601).
- **B**: [frame.rs:697:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L697), [state.rs:2165:18](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2165).

### `sm_inbound_completion` — A
- **N**: [state.rs:1979:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1979).
- **M**: [connection.rs:2143:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2143), [connection.rs:2147:5](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2147), [connection.rs:2193:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2193), [connection.rs:2233:5](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2233), [connection.rs:2240:5](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2240), [frame.rs:1797:13](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1797).
- **R**: [connection.rs:2074:8](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2074), [connection.rs:2174:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2174), [connection.rs:2177:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2177), [connection.rs:2178:15](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2178), [connection.rs:2183:31](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2183), [connection.rs:2199:11](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2199), [connection.rs:2206:27](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2206), [connection.rs:2225:8](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2225), [connection.rs:2227:23](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2227), [connection.rs:2328:8](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2328), [connection.rs:2396:12](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2396), [frame.rs:1595:41](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1595).
- **B**: [batch_write.rs:780:30](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L780), [batch_write.rs:865:18](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L865), [frame.rs:658:14](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L658), [frame.rs:698:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L698), [frame.rs:1782:22](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1782), [frame.rs:1804:14](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1804), [stream_management/registration.rs:103:68](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L103).

### `inbound_frame_terminal` — A/T
- **N**: [state.rs:1980:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1980).
- **W**: [frame.rs:667:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L667), [frame.rs:1754:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1754).
- **M**: [connection.rs:1801:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1801), [connection.rs:2370:13](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2370).
- **B**: [frame.rs:699:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L699).

### `ordered_relay_handoff_tx` — T
- **N**: [state.rs:1981:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1981).
- **W**: [connection.rs:493:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L493).
- **B**: [frame.rs:700:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L700).

### `carbons_enabled` — A/I
- **N**: [state.rs:1982:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1982).
- **W**: [stream_management/registration.rs:496:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L496).
- **R**: [cleanup.rs:929:31](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L929), [cleanup.rs:2287:26](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2287), [registration.rs:656:16](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L656), [registration.rs:874:27](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L874).
- **B**: [frame.rs:701:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L701).

### `roster_interested` — A/I
- **N**: [state.rs:1983:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1983).
- **W**: [stream_management/registration.rs:497:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L497).
- **R**: [cleanup.rs:971:32](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L971), [cleanup.rs:2288:28](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2288), [registration.rs:659:16](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L659).
- **B**: [frame.rs:708:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L708).

### `blocklist_interested` — A/I
- **N**: [state.rs:1984:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1984).
- **W**: [stream_management/registration.rs:498:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L498).
- **R**: [cleanup.rs:972:35](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L972), [cleanup.rs:2289:31](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2289), [registration.rs:661:9](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L661).
- **B**: [frame.rs:709:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L709).

### `presence_available` — I
- **N**: [state.rs:1985:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1985).
- **W**: [stream_management/registration.rs:499:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L499).
- **R**: [cleanup.rs:2290:29](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2290), [registration.rs:405:8](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L405).
- **B**: [frame.rs:702:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L702).

### `presence_show` — I
- **N**: [state.rs:1986:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1986).
- **W**: [stream_management/registration.rs:500:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L500).
- **R**: [cleanup.rs:2291:24](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2291), [registration.rs:411:13](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L411).
- **B**: [frame.rs:703:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L703).

### `presence_status` — I
- **N**: [state.rs:1987:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1987).
- **W**: [stream_management/registration.rs:501:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L501).
- **R**: [cleanup.rs:2292:26](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2292), [registration.rs:415:13](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L415).
- **B**: [frame.rs:704:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L704).

### `presence_priority` — I
- **N**: [state.rs:1988:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1988).
- **W**: [stream_management/registration.rs:502:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L502).
- **R**: [cleanup.rs:2293:28](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2293), [registration.rs:407:61](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L407), [registration.rs:416:13](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L416).
- **B**: [frame.rs:705:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L705).

### `presence_payloads` — I
- **N**: [state.rs:1989:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1989).
- **M**: [stream_management/registration.rs:503:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L503).
- **R**: [cleanup.rs:2294:28](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2294), [registration.rs:422:13](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L422).
- **B**: [frame.rs:706:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L706).

### `pending_subscribes_flushed` — I
- **N**: [state.rs:1990:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1990).
- **W**: [stream_management/registration.rs:504:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L504).
- **R**: [cleanup.rs:2295:37](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2295), [registration.rs:714:19](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L714).
- **B**: [frame.rs:707:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L707).

### `pending_resume_stream_id` — A/T
- **N**: [state.rs:1991:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1991).
- **W**: [cleanup.rs:898:13](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L898), [stream_management/registration.rs:334:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L334).
- **M**: [connection.rs:2120:30](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2120), [stream_management/registration.rs:64:30](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L64).
- **R**: [connection.rs:1880:20](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1880), [connection.rs:1884:21](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1884), [registration.rs:460:37](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L460), [registration.rs:497:19](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L497).
- **B**: [frame.rs:710:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L710).

### `pending_resume_h` — A/T
- **N**: [state.rs:1992:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1992).
- **W**: [cleanup.rs:899:13](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L899), [connection.rs:2121:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2121), [stream_management/registration.rs:335:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L335).
- **M**: [stream_management/registration.rs:115:20](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L115).
- **B**: [frame.rs:711:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L711).

### `pending_resume_claim` — A/T
- **N**: [state.rs:1993:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1993).
- **M**: [cleanup.rs:900:18](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L900), [connection.rs:2122:14](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2122), [stream_management/registration.rs:309:26](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L309), [stream_management/registration.rs:331:26](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L331).
- **B**: [frame.rs:712:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L712).

### `pending_finalized_resume_outcome` — T
- **N**: [state.rs:1994:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1994).
- **W**: [registration.rs:870:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L870), [stream_management/registration.rs:337:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L337).
- **M**: [connection.rs:2012:32](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2012).
- **B**: [frame.rs:713:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L713).

### `registry_owner` — A
- **N**: [state.rs:1999:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1999).
- **W**: [registration.rs:257:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L257), [registration.rs:673:5](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L673), [registration.rs:814:13](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L814), [stream_management/registration.rs:453:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L453), [stream_management/registration.rs:534:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L534).
- **R**: [batch_write.rs:336:22](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L336), [cleanup.rs:894:12](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L894), [cleanup.rs:902:27](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L902), [cleanup.rs:1516:19](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1516), [cleanup.rs:1842:34](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1842), [cleanup.rs:2618:47](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2618), [connection.rs:1110:68](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1110), [connection.rs:1907:72](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1907), [frame.rs:1810:42](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1810), [state.rs:2167:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2167).
- **B**: [frame.rs:719:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L719).

### `socket_lifecycle` — A/T
- **N**: [state.rs:2000:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2000).
- **W**: [registration.rs:680:9](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L680).
- **M**: [stream_management/registration.rs:457:30](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L457).
- **R**: [cleanup.rs:1741:42](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1741).

### `recovered_pending_endpoint` — T
- **N**: [state.rs:2001:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2001).
- **W**: [stream_management/registration.rs:403:21](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L403).
- **M**: [registration.rs:634:36](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L634).
- **R**: [registration.rs:482:32](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L482), [registration.rs:489:32](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L489), [stream_management/registration.rs:400:20](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L400).

### `resume_endpoint_handoff_blocked` — T
- **N**: [state.rs:2002:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2002).
- **W**: [stream_management/registration.rs:405:21](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L405), [stream_management/registration.rs:415:17](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L415), [stream_management/registration.rs:446:13](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L446).
- **R**: [registration.rs:471:8](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L471), [stream_management/registration.rs:401:25](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L401).

### `write_interruption` — T
- **N**: [state.rs:2003:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2003).
- **R**: [batch_write.rs:635:8](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L635), [batch_write.rs:666:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L666), [batch_write.rs:686:17](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L686), [batch_write.rs:708:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L708), [batch_write.rs:855:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L855), [batch_write.rs:1053:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1053), [batch_write.rs:1066:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1066), [connection.rs:486:59](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L486), [registration.rs:649:32](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L649).
- **H**: [state.rs:2029:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2029).

### `live_resume_control` — T
- **N**: [state.rs:2004:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2004).
- **R**: [batch_write.rs:347:30](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L347), [batch_write.rs:486:33](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L486), [batch_write.rs:664:46](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L664), [connection.rs:484:35](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L484), [registration.rs:650:48](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L650).
- **H**: [batch_write.rs:1049:21](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1049), [cleanup.rs:1742:9](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1742), [cleanup.rs:1765:5](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1765).

### `suppress_sm_record_next_batch` — T
- **N**: [state.rs:2005:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2005).
- **W**: [connection.rs:1988:9](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1988), [connection.rs:2390:13](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2390), [stream_management/registration.rs:505:5](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L505).
- **R**: [connection.rs:1987:21](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1987), [connection.rs:2389:25](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2389).
- **B**: [frame.rs:716:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L716).

### `pending_sm_enable_commit` — A/T
- **N**: [state.rs:2006:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2006).
- **M**: [connection.rs:2119:10](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2119), [state.rs:2158:28](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2158).
- **B**: [frame.rs:717:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L717).

### `deferred_inbound` — T
- **N**: [state.rs:2007:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2007).
- **M**: [batch_write.rs:798:21](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L798), [batch_write.rs:861:9](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L861), [batch_write.rs:1138:21](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1138), [connection.rs:755:42](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L755), [connection.rs:2355:28](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2355), [connection.rs:2447:5](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2447).
- **R**: [batch_write.rs:647:35](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L647), [batch_write.rs:680:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L680), [batch_write.rs:850:29](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L850), [batch_write.rs:1059:12](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L1059), [connection.rs:622:37](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L622), [connection.rs:2446:19](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2446).

### `sm_recovery_required` — A/T
- **N**: [state.rs:2008:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2008).
- **W**: [cleanup.rs:2776:5](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2776), [state.rs:2040:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2040), [state.rs:2115:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2115).
- **R**: [batch_write.rs:975:31](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L975), [cleanup.rs:838:23](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L838), [cleanup.rs:875:13](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L875), [cleanup.rs:891:12](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L891), [cleanup.rs:895:16](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L895), [cleanup.rs:915:16](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L915), [cleanup.rs:934:13](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L934), [cleanup.rs:1015:16](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1015), [cleanup.rs:1843:9](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1843), [cleanup.rs:2619:13](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2619), [connection.rs:2338:8](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2338), [connection.rs:2441:5](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2441), [state.rs:2037:12](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2037), [state.rs:2112:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2112).

### `terminal_sm_recovery` — A
- **N**: [state.rs:2009:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2009).
- **W**: [state.rs:2052:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2052).
- **M**: [batch_write.rs:988:21](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L988), [state.rs:2083:17](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2083), [cleanup.rs:2425:28](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L2425).
- **R**: [batch_write.rs:976:30](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L976), [batch_write.rs:986:32](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L986), [connection.rs:2182:32](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2182), [connection.rs:2205:28](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2205), [connection.rs:2360:28](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2360), [connection.rs:2442:12](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2442), [replay.rs:687:9](../../../server/crates/waddle-server/src/server/routes/websocket/replay.rs#L687), [state.rs:2079:12](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2079), [state.rs:2096:25](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2096), [state.rs:2112:42](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2112).

### `pending_replay_completions` — A/T
- **N**: [state.rs:2010:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2010).
- **M**: [connection.rs:2545:13](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2545), [connection.rs:2560:63](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2560).
- **R**: [connection.rs:2537:34](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2537), [connection.rs:2555:8](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2555), [connection.rs:2565:20](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L2565).

### `terminal_sm_recovery_dropped` — T
- **N**: [state.rs:2011:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2011).
- **W**: [state.rs:2044:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2044), [state.rs:2080:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2080).
- **R**: [state.rs:2080:49](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2080), [state.rs:2092:53](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2092), [state.rs:2103:23](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2103).

### `terminal_sm_recovery_drop_warned` — T
- **N**: [state.rs:2012:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2012).
- **W**: [state.rs:2045:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2045), [state.rs:2106:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2106).
- **R**: [state.rs:2092:12](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2092).

### `send_window_pause_deadline` — T
- **N**: [state.rs:2013:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2013).
- **W**: [connection.rs:575:21](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L575), [connection.rs:610:17](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L610).
- **R**: [connection.rs:573:35](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L573), [connection.rs:612:46](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L612).

### `send_window_last_request_acked` — T
- **N**: [state.rs:2014:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2014).
- **W**: [connection.rs:590:21](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L590).
- **R**: [connection.rs:587:64](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L587).

### `state_machine` — A/T
- **N**: [state.rs:2015:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2015).
- **W**: [state.rs:2188:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2188), [state.rs:2236:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2236).
- **B**: [cleanup.rs:948:17](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L948), [cleanup.rs:1236:25](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L1236), [connection.rs:554:27](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L554), [connection.rs:937:36](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L937), [connection_actor_core.rs:208:18](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L211), [frame.rs:718:9](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L718), [frame.rs:1698:23](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L1698), [outbound.rs:225:28](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L225), [replay.rs:344:32](../../../server/crates/waddle-server/src/server/routes/websocket/replay.rs#L344), [state.rs:2195:27](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2195), [state.rs:2251:27](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2251).

### `keepalive_config` — T
- **N**: [state.rs:2016:13](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2016).
- **W**: [state.rs:2185:9](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2185).
- **R**: [state.rs:2233:33](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2233), [stream_management/registration.rs:489:21](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L489).

### Reconciling a refreshed compiler pass

The `5763923e` captures have been reconciled as described above; future compiler
output supersedes those coordinates. For each profile, retain
the old exact key `(tag, file, line, column, compiled target kind/name/test)`
and identify the field by its declaration file plus field name, not by a
possibly shifted line number embedded in the tag. Map the old source position
through the base-to-new diff and verify the full source span, enclosing
function, target and column before matching the corresponding fresh row.
If a line changed, moved ambiguously, or yields multiple candidates, inspect it
individually; do not carry the old operation label forward automatically.
Compare both key sets and report additions, removals and unmatched/multiply
matched rows. Recheck each B/H path, newly marked declaration and feature gate
against current source; the two profiles can diverge. Only then replace the
old links and counts. This procedure reconciles compiler spans, not uses
hidden behind unmarked aliases or all possible runtime paths.
