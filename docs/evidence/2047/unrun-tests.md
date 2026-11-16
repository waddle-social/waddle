# Issue 2047: verification checklist

This is the verification phase's checklist for
[#2047](../../planning/federation-2047.md#verification-phase). It lists every
test target that `integration/2047` adds or changes relative to `origin/main`,
who owns it, what it needs to run and whether it has been executed. It was
built from the merged tree at `73fd797d9`; the code at that commit is the code
at merge `6e4cceebb`, because the three commits after it change only
`server/docs/planning/authority-inventory.json` and
`production-authority-inventory.md`.

Slice slugs follow the plan: W1 authorization ID codec, W2 destination
classifier, W3 client identity, W4a S2S codec, W4b verifier, W4c connection
manager, W4d discovery and dialback, W5a routing and reachability, W5b first
host-grant slice, W5c writer fence, W5i identity profile, W5o outcast
precedence and schema version, W5r restore incarnation, W6b outbound S2S
rooms (merged at `9766d9c81`), W5d revocation fence (merged at
`46187e69f`) and W5e remote principals at the effect boundary (merged at
`763988ff0`); the rows those merges add or change describe the merge and
the commits after it. W6a is inbound S2S rooms: its rows describe that
slice's branch, whose targets were compiled and not run. A target lists every slice that touched it,
the first being the slice that created it; "integration" is a commit made on
the integration branch itself.

## How to read the status

- **Run** states the cases that passed, the run log that shows it and the
  commit it ran at. It covers only that log's selection on that tree.
- **Not run** means no run on this tree is recorded, whether or not the
  target compiles. Slice gates compiled the new integration targets with
  `cargo check`; none executed them except as listed under "Run".
- A PostgreSQL-gated case returns early with a `skipping ...` line when
  `WADDLE_TEST_POSTGRES_URL` is unset, and nextest reports that as a pass.
  Set `WADDLE_TEST_REQUIRE_POSTGRES=1` in verification so a missing database
  fails the case. A SpiceDB-gated case is `#[ignore]` and runs only with
  `--run-ignored all` and `WADDLE_TEST_SPICEDB_ENDPOINT`,
  `WADDLE_TEST_SPICEDB_PRESHARED_KEY` and `WADDLE_TEST_SPICEDB_DISPOSABLE=1`
  (every such case empties the SpiceDB it is given).
- "TestServer process" means the case starts the `waddle-server` binary as a
  child process on loopback ports through `waddle-ws-test-support`. "Loopback"
  means in-process sockets on `127.0.0.1` with no external service.

## Execution evidence

| Source | What it shows |
| --- | --- |
| `integration-run2-s2s.log` | nextest on integration at `6e4cceebb`: `waddle-xmpp` 133 of 133 passed across 4 binaries, `waddle-server` 361 of 361 passed across 11 binaries; no skips reported. |
| `integration-run5-s2s.log` | nextest on integration at `b887a2941` (the W6b merge, the inventory refresh and the PostgreSQL account-bound cases), with `WADDLE_TEST_REQUIRE_POSTGRES=1` and PostgreSQL 17.11: `waddle-xmpp` 133 of 133 passed across 4 binaries; `waddle-server` 426 of 427 passed across 15 binaries. The failure, `s2s_connection_manager` `limits::a_full_backoff_table_neither_refuses_a_healthy_destination_nor_flushes_a_wait`, was an outbound attempt that told its dialler of its failure before releasing its dial slot, so an immediate retry was refused as in progress; fixed in `9313de477`. |
| `integration-run5-s2s-rerun.log` | the same selection at `9313de477`: `waddle-xmpp` 133 of 133 and `waddle-server` 427 of 427 passed, the two PostgreSQL cases of `xep0045_s2s_outbound_muc` included; no skips reported. |
| `integration-run5-s2s-final.log` | the same selection at `e4ef00870`, which records a dial failure before freeing the dial slot: `waddle-xmpp` 133 of 133 and `waddle-server` 427 of 427 passed, the two PostgreSQL cases included; no skips reported. |
| `integration-run5-s2s-stress.log` | `s2s_connection_manager`, `s2s_stream_end`, `xep0045_s2s_outbound_muc` and `xep0410_s2s_self_ping` at `9313de477`, five consecutive nextest runs, PostgreSQL required: 5 of 5 runs passed. |
| `integration-run2-hostgrant-b.log` | nextest of the first host-grant slice's set: 110 of 110 passed across 8 binaries, including the `#[ignore]` SpiceDB cases. |
| `integration-run2-hostgrant-c.log` | nextest of the writer fence's set: 64 of 64 passed across 6 binaries. |
| `integration-run4-c.log` | nextest of the writer fence's set at `def22148d` (the commit is the log's first line): 66 of 66 passed across 6 binaries, including the `#[ignore]` SpiceDB cases. |
| `integration-run4-r.log` | nextest of the restore incarnation's set at `def22148d`: 114 of 114 passed across 6 binaries (`host_grant_authority`, `host_grant_projection`, `host_grant_restore`, `restricted_runtime_startup`, `xep0045_host_grant_admission`, `xep0045_host_grant_audience`), including the `#[ignore]` SpiceDB cases. |
| `integration-run6-hostgrant.log` | nextest at `c415ea213` (after the W5d and W5e merges and the fixes that followed them) of 21 binaries: the revocation fence's focused set, the effect boundary's suite and every host-grant, authorization schema and codec binary, with `--run-ignored all`, `WADDLE_TEST_REQUIRE_POSTGRES=1`, PostgreSQL 17.11 and SpiceDB 1.56.1 on the **PostgreSQL datastore** (a database of its own on the shared cluster, dropped afterwards): 277 of 278 passed. The failure, `xep0045_host_grant_revocation` `the_sweep_removes_a_remote_occupant_only_when_no_stream_would_admit_it`, was the new case waiting for an arrival presence that a member seated through its room actor never sends; fixed in the case at `f05fed7ac`. |
| `integration-run6-hostgrant-final.log` | the same 21 binaries as `integration-run6-hostgrant.log` at `ff1dbf666`, the integration tree's last code and test change included, with the same environment and a fresh PostgreSQL-backed SpiceDB: 278 of 278 passed; no skips reported. |
| `integration-run6-hostgrant-rerun.log` | `xep0045_host_grant_revocation` at `f05fed7ac`, PostgreSQL required: 12 of 12 passed, its two-process case included. |
| `integration-run6-sweep-control.log` | the new sweep case at `f05fed7ac` with the sweep's rule for remote occupants (`6f330e5e2`) reverted in the working tree: it fails where the remote member is removed without stream evidence. |
| `integration-run6-drop-control.log` | `live_spicedb_an_increase_whose_initiator_is_no_longer_trusted_is_dropped` at `f05fed7ac` with reconciliation's drop disabled in the working tree, against a memory SpiceDB: it fails where the remote initiator's increase is activated. |
| `integration-run6-w5b.log` | the first host-grant slice's focused set by `live/run-int-w5b-focused.sh` at `f05fed7ac`, memory SpiceDB: 111 of 111 passed across 8 binaries. |
| `integration-run6-s2s.log` | the S2S selection of the run5 logs at `f05fed7ac`, the W6b binaries included, PostgreSQL required with a database of its own: `waddle-xmpp` 133 of 133 and `waddle-server` 427 of 427 passed; no skips reported. |
| `server/docs/planning/host-grant-authority-2047.md` | Records the first slice's set passing 110 of 110 on 2026-11-14 (two consecutive runs; an earlier run of that revision failed one admission case with an ingress substrate timeout, which passed alone and in both later runs; cause unconfirmed) and again on 2026-11-15 at `ef5205cf2`, and the writer fence's set passing 64 of 64 on 2026-11-15 at `ef5205cf2`. PostgreSQL 17.11 and SpiceDB 1.56.1 with the memory datastore. |

The two host-grant logs do not record the commit they ran at or the
PostgreSQL and SpiceDB versions; the design document names `ef5205cf2` and
those versions for its own runs. The logs are evidence for the binaries they
selected, not for the whole tree.
Runs before run6 used SpiceDB's memory datastore, so their `live_spicedb_`
and `host_grant_projection` cases are evidence against SpiceDB's API and
consistency semantics only. `integration-run6-hostgrant.log` ran every
`live_spicedb_` case of its 21 binaries against SpiceDB on the PostgreSQL
datastore; `integration-run6-w5b.log` ran the first slice's set on the memory
datastore again at the later commit.

The second-process cases of `host_grant_projection` (a replica process that
stops at each stage) and `xep0045_host_grant_revocation` (a replica that
stops running its fence), and the cases of `host_grant_revocation_fence` and
`account_jid_retirement` that run the `waddle-server` binary's operator
subcommands, ran in run6; no run covers Prosody, ejabberd,
the Rust library unit tests of `waddle-server`, `bun test` for `chat`, or the
`clustering` feature.

## Integration test targets

### waddle-xmpp (`server/crates/waddle-xmpp/tests/`)

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `s2s_codec` (54 cases) | W4a, W4c, W5a | none | Run 54/54, s2s, run5 final and run6 logs |
| `s2s_pre_authentication` (16) | W4c, W5a, W4d, integration | none | Run 16/16, s2s, run5 final and run6 logs |
| `s2s_stream_addresses` (16) | W4c, W5a, W4d, integration | none | Run 16/16, s2s, run5 final and run6 logs |
| `xep0178_s2s_negotiation` (47) | W4a, W4c, W4d, W5a, integration | none | Run 47/47, s2s, run5 final and run6 logs |
| `identity_profile` (19) | W5i | none (loopback address checks only) | Not run |
| `xep0313_mam` (edited, 11) | W5c | none; two cases need PostgreSQL and skip without `WADDLE_TEST_POSTGRES_URL` | Not run |
| `xep0402_pep_native_bookmarks` (edited, 15) | W5i | none | Not run |
| `xep0513_mentions` (edited, 20) | W5c | none | Not run |

### waddle-server, S2S and destination routing (`server/crates/waddle-server/tests/`)

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `s2s_verifier` (162 cases in 11 modules) | W4b | none (in-memory rustls handshakes) | Run 162/162, run5 final and run6 logs |
| `s2s_connection_manager` (54 in 3 modules) | W4c, W4d, W6b | loopback | Run 54/54, run5 final and run6 logs |
| `xep0220_s2s_dialback` (39 in 4 modules) | W4d | loopback | Run 39/39, run5 final and run6 logs |
| `xep0368_s2s_discovery` (37 in 5 modules) | W4d | loopback; `SystemDnsLookup` cases use a loopback name server | Run 37/37, run5 final and run6 logs |
| `s2s_routing` (15) | W5a, integration, W6b | loopback | Run 15/15, run5 final and run6 logs |
| `s2s_iq_correlation` (13) | W5a | loopback | Run 13/13, run5 final and run6 logs |
| `s2s_reachability` (8) | W5a, W6b, W6a | loopback | Not run with W6a's changes: two cases added (the MUC service with and without a room host), every call of `reach` changed, and `muc#admin`, `muc#owner` and XEP-0425 requests now reach the room host. 6/6 before them, run5 final and run6 logs |
| `s2s_d7_refusal` (4) | W5a | none | Run 4/4, run5 final and run6 logs |
| `xep0030_s2s_server_discovery` (6) | W5a, W6b | loopback | Run 6/6, run5 final and run6 logs |
| `xep0199_s2s_ping` (3) | W5a | loopback | Run 3/3, run5 final and run6 logs |
| `destination_classifier` (24) | W2, W5i, W5a | loopback | Run 24/24, run5 final and run6 logs |
| `xep0045_s2s_outbound_muc` (41) | W6b, integration | loopback; in-memory SQLite, and two cases on PostgreSQL that skip without `WADDLE_TEST_POSTGRES_URL` | Run 41/41, run5 final, stress and run6 logs |
| `xep0410_s2s_self_ping` (4) | W6b | loopback | Run 4/4, run5 final, stress and run6 logs |
| `s2s_stream_end` (12) | W6b | loopback | Run 12/12, run5 final, stress and run6 logs |
| `xep0280_remote_room_carbons_ws` (7) | W6b | loopback (production `/ws` router in process) | Run 7/7, run5 final and run6 logs |
| `xep0045_s2s_inbound_muc` (29) | W6a | loopback (production `/ws` router, S2S router and connection manager in process); in-memory SQLite, and five cases on PostgreSQL 17 that skip without `WADDLE_TEST_POSTGRES_URL` (the managed room: a member entering, a revoked member, a room that is not federated, a remote admin's affiliation change through the store, a member refused administration) | Not run |
| `xep0045_s2s_inbound_muc_replicas` (6) | W6a | loopback; two replicas in one process, each with its own in-memory SQLite, S2S router and connection manager, and a carrier in place of the cluster's room claims and relay. Compiled with and without `clustering` | Not run |
| `xep0313_s2s_inbound_mam` (5) | W6a | loopback; in-memory SQLite, and one case on PostgreSQL 17 that skips without `WADDLE_TEST_POSTGRES_URL` | Not run |
| `xep0030_s2s_inbound_muc_discovery` (6) | W6a | loopback; in-memory SQLite | Not run |
| `xep0085_s2s_inbound_chat_states` (1) | W6a | loopback; in-memory SQLite | Not run |
| `xep0115_s2s_inbound_caps` (1) | W6a | loopback; in-memory SQLite | Not run |
| `xep0308_s2s_inbound_corrections` (2) | W6a | loopback; in-memory SQLite | Not run |
| `xep0333_s2s_inbound_markers` (1) | W6a | loopback; in-memory SQLite | Not run |
| `xep0359_s2s_inbound_stanza_ids` (1) | W6a | loopback; in-memory SQLite | Not run |
| `xep0424_s2s_inbound_retraction` (2) | W6a | loopback; in-memory SQLite | Not run |
| `xep0425_s2s_inbound_moderation` (2) | W6a | loopback; in-memory SQLite | Not run |
| `xep0444_s2s_inbound_reactions` (2) | W6a | loopback; in-memory SQLite | Not run |
| `xep0461_s2s_inbound_replies` (2) | W6a | loopback; in-memory SQLite | Not run |
| `clustering_remote_room_request` (3) | W6a | none; builds with the `clustering` feature only (`--features test-support,clustering`) | Not run |
| `rfc6120_destination_routing_ws` (7) | W2, W5a | TestServer process | Not run |
| `rfc3921_session_destination_ws` (edited, 1) | W2 | TestServer process | Not run |
| `xep0030_discovery_destination_ws` (edited, 2) | W2 | TestServer process | Not run |
| `xep0199_ping_ws` (edited, 1) | W2 | TestServer process | Not run |
| `xep0280_carbons_destination_ws` (edited, 1) | W2 | TestServer process | Not run |
| `dm_pinned_messages_ws` (edited, 13) | W5a | TestServer process | Not run |
| `xep0054_0049_0191_ws` (edited, 27) | W5a | TestServer process | Not run |

### waddle-server, host grants, authorization schema and codec

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `host_grant_authority` (21) | W5b, W5c, W5d, W5e | PostgreSQL 17 (superuser login; one case creates a role) | Run 21/21, run6 host-grant final and run6 W5b logs |
| `xep0045_host_grant_admission` (15) | W5b, W5c, W5e | PostgreSQL 17 | Run 15/15, run6 host-grant final and run6 W5b logs |
| `xep0045_host_grant_audience` (4) | W5b, W5c | PostgreSQL 17 | Run 4/4, run6 host-grant final and run6 W5b logs |
| `host_grant_projection` (11) | W5b, W5c, W5d | PostgreSQL 17 and SpiceDB; `live_spicedb_` cases are `#[ignore]`; one case runs replicas as separate processes | Run 11/11, run6 host-grant final log (PostgreSQL datastore) and run6 W5b log (memory) |
| `authz_schema_version` (24; 11 `#[ignore]`) | W5o, W5b | SpiceDB for the ignored cases | Run 24/24, run6 host-grant final and run6 W5b logs |
| `authz_id_codec` (12; 1 `#[ignore]`) | W1, W5o, W5i, integration | SpiceDB for the ignored case | Run 12/12, run6 host-grant final and run6 W5b logs |
| `authz_codec_incarnation` (11; 5 `#[ignore]`) | W1, W5o | SpiceDB for the ignored cases | Run 11/11, run6 host-grant final and run6 W5b logs |
| `channel_outcast_precedence` (13; 5 `#[ignore]`) | W5o, W5b | SpiceDB for the ignored cases | Run 13/13, run6 host-grant final and run6 W5b logs |
| `xep0045_host_grant_fence` (29 in 2 modules) | W5c, integration, W5d | PostgreSQL 17; loopback | Run 29/29, run6 host-grant final log |
| `host_grant_enrolled_set` (9) | W5c, integration | PostgreSQL 17 | Run 9/9, run6 host-grant final log |
| `spaces_pubsub_owner_seed` (9) | W5c, W5e | none (in-memory state) | Run 9/9, run6 host-grant final log |
| `xep0503_channel_placement` (6) | W5c | none (in-memory state, loopback) | Run 6/6, run6 host-grant final log |
| `xep0060_extension_route_room_access` (3) | W5c | none (in-memory state, loopback) | Run 3/3, run6 host-grant final log |
| `host_grant_restore` (46; 28 `#[ignore]`) | W5r, W5d, W5e | PostgreSQL 17 and SpiceDB for the ignored cases | Run 46/46, run6 host-grant final log |
| `restricted_runtime_startup` (18 in 3 modules; a fourth, `clustered`, builds with the `clustering` feature only) | W5r | PostgreSQL 17 (superuser login; the cases create roles, and one starts the server under the runtime login) | Run 18/18 without `clustering`, run6 host-grant final log |
| `host_grant_revocation_fence` (7) | W5d | PostgreSQL 17; one case runs the `waddle-server` binary's operator subcommands | Run 7/7, run6 host-grant final log |
| `xep0045_host_grant_revocation` (14) | W5d, integration, W6a | PostgreSQL 17; loopback; one case runs a replica as a second process | Not run with W6a's changes: the sweep's case for a remote occupant is replaced by three (a domain that is not admitted, effective standing with a pending increase, a closed scope). 12/12 before them, run6 host-grant final log |
| `account_jid_retirement` (10) | W5d | PostgreSQL 17 | Run 10/10, run6 host-grant final log |
| `xep0060_space_fanout_fence` (2) | W5d | PostgreSQL 17 (the #2056 control) | Run 2/2, run6 host-grant final log |
| `xep0410_host_grant_self_ping` (2) | W5d | PostgreSQL 17; loopback | Run 2/2, run6 host-grant final log |
| `host_grant_remote_principals` (16; 3 `#[ignore]`) | W5e, integration, W6a | PostgreSQL 17; SpiceDB for the ignored cases | Not run with W6a's changes: two `live_spicedb_` cases added (an increase that activates while its initiator's home is trusted, and one dropped because the home is no longer admitted). 14/14 before them, run6 host-grant final log |
| `spicedb_architecture_guard` (edited, 4) | W5o | none (reads source; names SpiceDB variables only in messages) | Not run |
| `ingress_relation_guard_lab` (edited, 8) | W5b | PostgreSQL (17 for the definer API pilot); skips without `WADDLE_TEST_POSTGRES_URL` | Not run |
| `migration_owner_lab` (edited, 5) | W5b | PostgreSQL; skips without `WADDLE_TEST_POSTGRES_URL` | Not run |
| `xep0045_managed_affiliation_target_ws` (2) | W1 | TestServer process | Not run |

### waddle-server, identity profile and client-facing suites

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `identity_profile_accounts` (8) | W5i | loopback | Not run |
| `identity_profile_ws` (7) | W5i | TestServer process | Not run |
| `sasl1_identity_ws` (edited, 12) | W5i | loopback | Not run |
| `rfc6121_roster_ws` (edited, 12) | W5i | TestServer process | Not run |
| `xep0198_refused_stanza_ack_ws` (1) | W5i | TestServer process | Not run |
| `xep0492_dm_bookmarks_ws` (edited, 6) | W5i | none | Not run |
| `xep0503_spaces_ws` (edited, 9) | W5i | TestServer process | Not run |

### waddle-client-session

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `tests/identity.rs` (6) | W3 | none | Not run |

### Shared support modules (not targets)

`tests/host_grant_support/mod.rs` (W5b, W5e) and its `live_spicedb.rs`,
`tests/host_grant_revocation_support/mod.rs` (W5d),
`tests/xep0060_space_fanout_fence/storage.rs` (W5d),
`tests/ws_loopback_support/mod.rs` (W5c), `tests/s2s_routing_support/mod.rs`
(W5a, W6b, W6a), `tests/s2s_remote_muc_support/mod.rs` (W6b, integration)
and `tests/s2s_inbound_muc_support/mod.rs` with its `managed.rs` (W6a) are
compiled into the targets above and add no cases. The inbound MUC support
creates its rooms federated (its S2S snapshot sets
`[host_policy] federate_new_rooms`, and the managed fixture sets the
catalog's `federated` column); a case that needs the default clears it. W6a changed
`s2s_routing_support` and `host_grant_revocation_support` (a replica can
run an S2S snapshot), so the targets that include them were compiled again
and their recorded runs predate that change: `s2s_routing`,
`s2s_iq_correlation`, `s2s_d7_refusal`, `xep0045_s2s_outbound_muc`,
`xep0030_s2s_server_discovery`, `xep0199_s2s_ping`, `xep0410_s2s_self_ping`
and `xep0410_host_grant_self_ping`. The submodules of `s2s_verifier`,
`s2s_connection_manager`, `xep0220_s2s_dialback`, `xep0368_s2s_discovery` and
`host_grant_fence_cases/sinks.rs` are counted under their parent targets.

## Library unit test modules edited on the branch

These files hold existing cases that the branch edited, and one, the
owner-form module of `waddle-xmpp`, two cases W6a added. The
`waddle-server` modules are compiled by no slice gate (the library test
build is forbidden until verification), so none of them is known to
compile; the `waddle-xmpp` modules are compiled by that crate's Clippy
gate (`--all-targets`). All are step 3 of the verification phase.

| Module | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `waddle-server/src/admin/channel_inventory_tests.rs` | W5c | none | Not run |
| `waddle-server/src/admin/channels.rs` (`tests`) | W6a | none; one catalog record literal gained the `federated` field | Not run |
| `waddle-server/src/server/xmpp_channels.rs` (`tests`) | W6a | none; the catalog row literal gained the `federated` column | Not run |
| `waddle-xmpp/src/muc/owner/tests.rs` | W6a | none; two cases added (the `federated` field of the owner form, and a stored configuration without it) | Not run |
| `waddle-server/src/admin/space_catalog_cascade_tests.rs` | W5c | none | Not run |
| `waddle-server/src/auth/directory/tests.rs` | W5i | none | Not run |
| `waddle-server/src/auth/native/tests.rs` | W5i | none | Not run |
| `waddle-server/src/db/migrations/tests.rs` | W5b | PostgreSQL for the `postgres_` cases; they skip without `WADDLE_TEST_POSTGRES_URL` | Not run |
| `waddle-server/src/ingress/room_pin_tests.rs` | W5c | none | Not run |
| `waddle-server/src/permissions/schema/tests.rs` | W5o | none | Not run |
| `waddle-server/src/server/routes/websocket/tests/muc.rs` | W5c | none | Not run |
| `waddle-server/src/server/routes/websocket/tests/rfc3921_session_destination.rs` | W2 | none | Not run |
| `waddle-server/src/server/routes/websocket/tests/xep0030_discovery_destination.rs` | W2 | none | Not run |
| `waddle-server/src/server/routes/websocket/tests/xep0199_ping.rs` | W2 | none | Not run |
| `waddle-server/src/server/routes/websocket/tests/xep0280_carbons_destination.rs` | W2 | none | Not run |
| `waddle-server/src/server/tests.rs` | W5i, W5a | loopback | Not run |
| `waddle-xmpp/src/muc/room_registry_actor/tests.rs` | W5c | none | Not run |
| `waddle-xmpp/src/protocol/frame/tests.rs` | W5i | none | Not run |
| `waddle-xmpp/src/protocol/wire_trace_l4_room_tests.rs` | W5c | none | Not run |

The writer fence's design records two of these as the library cases that
cover its pin and unpin paths (`ingress/room_pin_tests.rs`) and the join's
parent repair from a link (`websocket/tests/muc.rs`).

## Chat tests (`chat/tests/`, `bun test`)

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `qualified-identity.test.ts` (9 cases) | W3 | Bun | Not run |
| `room-id-quarantine.test.ts` (edited, 15) | W3 | Bun | Not run |
| `helpers/mock-browser-storage.ts` (shared helper, not a target) | W3 | n/a | n/a |

## Summary

| | Count |
| --- | --- |
| Test targets listed | 98 (77 integration targets, 19 library test modules, 2 chat test files) |
| Run, with a cited log | 37 targets, 806 cases, all passed in their latest cited run |
| Not run | 61 targets (19 of them library modules) |

By log: the run6 S2S log covers 19 targets (133 `waddle-xmpp` cases in 4,
427 `waddle-server` cases in 15); the run6 host-grant final log covers 21 targets
(278 cases) and passed in full in its final run; the run6 W5b log repeats 8
of those 21 on the memory datastore.

A target is counted as run only when the whole binary's cases appear in a
log. Skipped prerequisites are not recorded as passed: nothing here was run
against Prosody, ejabberd or a second deployment.

W6a adds fourteen targets (63 cases) and changes the cases of three that
had run (`s2s_reachability`, `xep0045_host_grant_revocation` and
`host_grant_remote_principals`, 32 cases in the logs), which are counted as
not run until they run again: every case of W6a is compiled and none has
been executed. Its PostgreSQL cases are the only evidence of the seat
table, of a remote principal's message through the room's acceptance and
of a remote admin's affiliation change through the host-grant store, on
that engine. Its cases with two replicas run both in one process and hand
a carried request from one router to the other directly: the cluster's
room claims and the relay hop of
`waddle.clustering.relay.remote_room_request.v1` are compiled and not
exercised by any case, and need two processes with the `clustering`
feature (verification steps 6 and 8).

## Deferred hardening and follow-ups

Each item names the design document or slice that recorded it. None is
implemented unless the text says so.

### Items recorded by the known-issue list

- **Discovery resolver behaviour (W4d).** A CNAME-owned SRV record that
  hickory reads as a negative answer makes discovery fall back to A and AAAA
  lookups although an SRV record exists; discovery must not treat it as
  absent. Separately, cap hickory's `negative_max_ttl` to the 30 s memory the
  discovery design gives a failed lookup, so a longer negative TTL cannot
  extend it.
- **Verification cancellation (E1).** Tests that cancel a verification in
  flight and assert that the resources it held are released.
- **Lookup failure mapping (W5a).** Map `ResolveError::LookupFailed` to
  `remote-server-timeout` with type `wait`, not `remote-server-not-found`:
  a failed lookup is not evidence that the domain does not exist.
- **Writer-fence paths without a test (W5c).** Tests for the call-thread
  anchor (the fenced test state configures no media server, and the Muji
  refusal precedes the update the anchor is built from), the relayed ordered
  Muji presence path (only the local path is driven), the mirror repair loop
  and its generation check, and the transient publish the Spaces backfill no
  longer makes.
- **Writer-fence preconditions before the first enrollment (W5c, W5r;
  `host-grant-authority-2047.md`, "One decision for every surface").** The
  surfaces outside room-manifest acceptance read the replica's attachment
  only while its attachment gate is attached, so a re-verification in
  progress or failed closes them. Two differences from acceptance remain,
  and both must be closed before the first scope is enrolled: each surface
  decision compares the attachment's binding with the authority it reads
  and refuses on a difference, and each surface asks
  `HostGrantAttachmentGate::refuses_unmanaged` before serving a room no
  enrolled scope manages.
- **Reconciliation after a re-attachment (W5c, W5r).** The background
  reconciliation of a recorded affiliation change runs no pass while the
  attachment gate is not attached, and gives up after its last retry
  delay. The projection recovery pass runs only at a replica's first
  attachment, so such a change stays recorded and not effective until the
  scope's next reconciliation or the replica's next start; a recovery pass
  on every re-attachment would close that gap.
- **Client carry-overs from W3, for the client slice (W8).** Local-id caches
  that assume one client and must hold several; link generation with
  qualified slugs; `selectedSpaceId` keyed by `(service, node)`; and the
  TypeScript `bareJidKey`, which does not strip a trailing dot or apply
  nameprep.
- **Library unit tests edited but not compiled.** The 16 modules above were
  edited under slice gates that never build the `waddle-server` library test
  crate; the first compile is verification step 1 or 3.
- **`clustering` feature.** Slice gates before 2026-11-15 did not compile
  `waddle-server` with the `clustering` feature, although the release image
  builds with it. `cargo clippy -p waddle-server --lib --features clustering
  --locked -- -D warnings` passed on the integration tree at `73fd797d9`
  (library only; test targets and runs with the feature are still owed), and
  the slice gates include it from that point on.

### Handshake design (`s2s-handshake-2047.md`)

- B1 to B4 remain open until their evidence is run. The acceptance evidence
  it requires is a real two-Waddle-process lane with independent SpiceDB
  authorities, actual WSS for the local-progress assertions and test-only CA
  material that still runs the certificate, name and revocation checks;
  missing binaries or services fail that lane rather than skip it. The
  existing `TestServer` forces fixed-account provisioning and `ws://` and is
  not that fixture.
- The seven acceptance cases it lists (allowed pairs in both directions;
  certificate and identity rejections; header, SASL and downgrade attacks;
  fragmentation and resource exhaustion; restarts and rotation; default-off
  startup) have no two-process evidence. The in-process suites above cover
  the codec, negotiation and verifier parts only.
- Inbound direct TLS (XEP-0368) is deferred in the handshake and verifier
  designs until a two-stage verification design is reviewed.

### Verifier design (`s2s-verifier-2047.md`)

- SRVName name constraints are not implemented. A deployment whose CA relies
  on them cannot use SRV-IDs; an `otherName` constraint on an anchor or
  intermediate is refused.
- An internationalized name in a DNS-ID or SRV-ID is matched in A-label form
  only.
- A public-profile peer must present a DNS-ID for its source domain: a
  hosting provider's certificate naming a hosted domain only in an SRV-ID
  authenticates it on a private-CA profile only.
- Two-process authentication and drain evidence belongs to the connection
  manager and the interoperability harness (W10), not to the verifier's
  in-process set.

### Connection manager design (`s2s-connection-manager-2047.md`)

- W10 harness cases: a clustered ejabberd with `max_s2s_connections` above
  one, for the one-stream-per-pair restriction; a stanza whose `to` domain
  is in mixed case; and whether Prosody or ejabberd ever sends whitespace
  before a stream header, which this profile refuses.
- Measure the time a stanza at the 512 KiB ceiling takes when many streams
  share an exhausted global input rate, against the 30-second bound on one
  element.
- Activation (G20) and the switch of the default mode to `open` (D1): the
  socket-layer reserve (NB1), handshake monopolisation by a few source
  addresses and wildcard-certificate peers, and fairness of the dial rate
  between senders once routing dials on their behalf.

### Discovery design (`s2s-discovery-2047.md`)

- Discovery against real SRV records, including `_xmpps-server` targets, is
  outside the W10 harness: its peers are on loopback, which the address
  checks refuse for DNS-derived addresses. It needs peers at public
  addresses with published records and is decided with activation (G20);
  until then the stub suite and the loopback `SystemDnsLookup` tests are the
  evidence.
- Per-sender fairness of the dial rate (W5a and G20).
- DNSSEC validation, with DANE (D2's later slice).
- NB1 and the per-address limits at the switch to `open` and at G20.

### Dialback design (`s2s-dialback-2047.md`)

- W10: dialback in both directions against the pinned Prosody and ejabberd,
  including peers with mismatched certificates and peers that verify keys on
  an existing authenticated stream.
- Overlapping secrets for rotation without failed negotiations.
- Routing (W5a) and the effect boundary (G17) honour the `dialback` strength
  as D8 requires.
- Residual risk, accepted and not closed: keys are not bound to the TLS
  channel; the deployment secret authenticates every local domain to every
  peer that accepts dialback until it is rotated on every replica; dialback
  proves control of DNS and of the domain's server, not more.

### Routing design (`s2s-routing-2047.md`)

- Spaces (W7a): enable its handlers for remote principals in `reach`, the
  inventory and the reachability suite, and list it in server-domain
  `disco#items`.
- Inbound MUC (W6a), written and not run: a remote principal reaches only
  rooms whose `federated` setting is on, and the setting opens a room to
  every admitted domain, not to named ones; a request for a room another
  replica holds is carried there, and with two replicas dialling one peer
  their streams can alternate, which verification with two replicas has to
  show; a carried request whose hand-over has an unknown outcome is
  answered with an error and may still be decided; private messages,
  invitations, mentions, references and every payload outside the reviewed
  extensions are not enabled; D8 issuance covers the `muc#admin` writer and
  the admin command that sets a room affiliation, and not the Spaces
  relations (W7a); a remote occupant's copy, and a remote requester's
  administration answer, are recorded delivered when the router's queue
  takes them, and their interplay with the ingress recovery of unfinished
  deliveries and the room's effect outbox has not been executed; an
  unmanaged room's occupants at a domain the snapshot no longer admits
  stay until their stream is lost or the replica restarts; the request
  rate has no per-account share and the per-account archive allowance is
  kept in memory on one replica; MAM is answered and not advertised to
  remote askers; the web client does not offer the `federated` field.
- Outbound MUC (W6b), merged: a stream that waited for the router and ends
  for a deadline, eviction or replacement before reading again is reported
  as lost even when the peer wrote nothing more; end of file between
  elements, including a TLS end without `close_notify`, is closed, so a
  peer that crashed between stanzas looks like one dropping an idle
  connection and its occupants are not told.
- Account traffic (W6c): replace both D7 refusals for reviewed handlers,
  register inbound requests for correlation and define the ingress receipt
  for a remote target. A message carrying a DM pin marker for a remote peer
  must still be refused before it is routed: today the outbound D7 refusal
  keeps the marker off the wire. Check the sender's blocking list before
  offering messages and presence to remote JIDs.
- Effect-boundary admission (W5e): take the `RemotePrincipal` at the
  protected-effect boundary.
- Capability advertisement (W11): any further feature in server-domain
  `disco#info` for remote askers.
- Stanzas the interpreter routes to a remote JID other than IQs, such as a
  MUC mediated invitation to a remote account, are still dropped silently by
  local resource selection.
- Router metrics through the telemetry macros.

### Identity profile design (`identity-profile-2047.md`)

- Full RFC 7622 conformance needs an agreed PRECIS implementation and a
  versioned migration of stored names; validity disagreements with strict
  peers on non-ASCII names remain.
- Nested identities the C2S frame parser does not check: `to` and `from` in
  XEP-0297 `<forwarded/>`, XEP-0033 `<address/>` entries with a `uri`,
  XEP-0166 Jingle `initiator` and `responder`, and untyped data-form fields
  such as XEP-0313 `with`. A handler that reads one as an address must use
  `DataForm::bare_jid` or `parse_jid`.
- Addresses in other attributes or element text that are parsed with the
  folding `jid` parser: XEP-0272 `<muji room>`, the link-preview `<scope>`,
  and XEP-0372 and XEP-0513 mention `uri`. None writes a grant.
- UTS #46 mapping follows the Unicode version of the linked `idna` release;
  changing `idna`, `stringprep` or `jid` needs a profile version change.
- The offline `waddle-identity-audit` candidate (`candidate-ascii-account-v1`)
  excludes `+`; this profile's account set includes it.

### Host-grant authority design (`host-grant-authority-2047.md`)

- Revocation fence (W5d), decided in the design's "Revocation fence
  decisions": occupants a revoke leaves seated are removed by the sweep,
  and the queue lock in front of the scope's guard keeps overlapping
  acceptances from holding a revoke back. Open there: surfaces that decide
  before an await and act after it without deciding again (XEP-0425
  moderation, channel-wide mentions, Muji call start and media tokens, the
  owner configuration form, the extension bot list, the room and Space
  listings); an `UpdateConfig` that asks the decision's ticket in the room
  actor's turn; the admin commands' unbounded wait for a room's
  configuration lock; seats a lapsed replica keeps until it registers again;
  the relayed record of a remote membership; and the clock assumptions the
  fence's acknowledgement rests on (Linux replicas, PostgreSQL hosts kept in
  sync), which rollout checks and nothing enforces.
- A PubSub admission identity for serving an enrolled Space (needed before
  a scope's Space is served; W7a depends on it). Until then its node is
  described and listed to nobody.
- Remote principals at the effect boundary (W5e), decided in the design's
  "Remote principals at the effect boundary (G17)". W6a decides a seated
  remote occupant's deliveries, its removal by the revocation sweep and its
  re-seating after a claim move on the stream evidence its seat was
  admitted under, on the replica that holds the seat. Open there: a
  principal named on a replica that holds no seat for it is still decided
  without stream evidence (a trusted home's admin is then not counted as a
  reader); a per-scope `require_pkix`; D8 issuance for the Spaces
  relations (W7a), the one family of legacy privileged writers that does
  not ask whether the subject's home is trusted;
  an operation kind of its own for a dropped increase, which needs a
  migration of the kind constraint; and the rollout window in which a
  replica on an earlier snapshot can still activate an increase another
  replica would drop. Not covered by a test: the call mixer's domain in the
  issuance policy and in startup's check of the trusted-home list.
- Recreation of a scope, a room or a node under a new lifecycle.
- A deployment-wide policy guard: the first slice's community generation
  still does not fence a changed deployment override.
- Activation slice, optional: each replica records the start of the load
  that installed its enrolled set against its node lease, so activation can
  check readiness in a transaction instead of reading the gauge.
- Not covered by a test: the call-token gate; the media grant re-assertion
  paths; claim-move re-seating and the invitation rollbacks (they need
  actor-failure injection or clustering); the reconciliation retries; the
  host tool's item-read refusal (it needs a loaded extension); the
  channel-mention decision end to end in a managed room and its skip for a
  message without a channel mention; the refusal of a group-DM creation
  under a managed room's identifier; the exported gauges and the stale-set
  warning; the rule that a load finishing after a later-started one installs
  nothing; and the warning of a reload that holds fewer objects.
- No acceptance row of the writer fence yet uses two replicas; that is
  verification step 4.
- The blocking ledger entries B1 to B7 stay open until their gates are
  discharged by the verification steps the gate table names.
