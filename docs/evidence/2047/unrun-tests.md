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
`46187e69f`), W5e remote principals at the effect boundary (merged at
`763988ff0`), W6a inbound S2S rooms (merged at `e9cf20b46`) and W6c
account traffic (merged at `afdb0f35c`); the rows those merges add or
change describe the merge and the commits after it. The run7 logs below
ran at `4fd662777`. The W7a merge (`974735cda`) and the fixes after it
ran in the run8 rerun logs at `6ae0317a2`, the integration branch's head
when they ran; a row citing a run8 log describes that tree. A target lists every slice that touched it,
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
| `integration-run7-s2s.log` | nextest with `--profile ci` (slow-timeout 60 s, terminated after 10 periods; no retries) at `e51211a9a`, the W6a and W6c merges and the fixes after them, PostgreSQL required with a database of its own: `waddle-xmpp` 133 of 133 across 4 binaries; `waddle-server` 519 of 621 across 40 binaries. The 102 failures: every case of the inbound MUC suites' fixtures (`s2s_inbound_muc_support`, its scripted router) panicked loading an S2S snapshot for `example.com` under the default `.test` peer scope (`OutsideTestDomain`), and two `xep0160_s2s_account_offline_ws` cases looked for a detached session in the registry's memory while the test state keeps it in the durable tables. Its companion `integration-run7-s2s-clustering.log` is not evidence: the fixture fix was applied to the working tree while that build compiled. |
| `integration-run7-detached-control.log` | the two detached-session cases at `e51211a9a` with the account fixture's wait for the coordinator's unbind removed: they still fail, so the failure is the fixture's read of held stanzas, not that wait. |
| `integration-run7-s2s-rerun1.log` | the same selection at `f1c2f00fc` (both fixture fixes): `waddle-xmpp` 133 of 133, `waddle-server` 614 of 621. The seven: three cases of a trusted home's moderator role and an owner or admin standing for a seated account (the surfaces read no seat evidence without a canonical grant store, fixed in `55d5f89b6`), a managed room's remote admin standing refused as untrusted (the grant fixture's policy listed no trusted home), a non-canonical address the `jid` parser had already folded, a members-only join expected as `forbidden` where XEP-0045 §7.2.6 answers `registration-required`, and the archive bound case, which the router's own per-account and per-room bounds refused first. `integration-run7-s2s-clustering-rerun1.log`: 41 of 44, the three replica cases among the seven. |
| `integration-run7-s2s-rerun2.log` | the five inbound MUC binaries at `f1c2f00fc` (the commit its first line names) with the fixes later committed as `55d5f89b6`, `a1a81f25f` and `dd6babd30` in the working tree: 54 of 55 (the archive case); `integration-run7-s2s-clustering-rerun2.log` 44 of 44. `integration-run7-archive-bound-alone.log`: the archive case alone, three runs, 22 to 26 messages archived of a lower bound of 30, before the case raised the router's bounds. |
| `integration-run7-hostgrant.log` | the 21 host-grant binaries of the run6 logs (`account_jid_retirement` and `host_grant_remote_principals` among them) at `a0d4c434a`, `--run-ignored all`, PostgreSQL required, a PostgreSQL-backed SpiceDB 1.56.1 on 127.0.0.1:50064 (database `w2047_int_spicedb`, created and dropped by the run), `WADDLE_TEST_SPICEDB_DISPOSABLE=1`, `--profile ci`: 279 of 283. The four: two sweep cases of `xep0045_host_grant_revocation` whose snapshot named `.test`-outside domains under the default scope, a sink case that expected the moderator refusal's text before trusted homes could hold the role, and `restricted_runtime_startup` seeding a contact at a domain the deployment does not serve, which startup now keeps (G18-10). `integration-run7-hostgrant-rerun1.log`: those three binaries at `a0d4c434a` (the commit its first line names) with the fixes later committed as `53b396864`, `e4d1a94f4` and `4fceb7ea5` in the working tree, 61 of 61. |
| `integration-run7-gates-final.log` | the gates at `4fd662777`: fmt, clippy `waddle-xmpp` all targets, clippy `waddle-server` lib with and without `clustering`, `cargo check` of every target above with `test-support` and of the three clustering targets with `test-support,clustering`, the structure, typed-entry, PostgreSQL-env and authority-inventory scripts, and `check-poison-recovery.py` with the same findings as `ab832c1f5`. |
| `integration-run7-s2s-final.log` | the run7 S2S selection at `4fd662777`: `waddle-xmpp` 133 of 133 across 4 binaries, `waddle-server` 621 of 621 across 40 binaries, no case skipped. `integration-run7-s2s-clustering-final.log`: `clustering_remote_room_request`, `xep0045_s2s_inbound_muc_replicas` and `xep0045_s2s_inbound_muc_router` with `--features test-support,clustering`, 44 of 44. |
| `integration-run7-hostgrant-final.log` | the run7 host-grant selection at `4fd662777`, as `integration-run7-hostgrant.log`: 283 of 283 across 21 binaries, the `#[ignore]` SpiceDB cases included. |
| `integration-run8-gates.log` | the gates at `974735cda`, the W7a merge: fmt; clippy `waddle-xmpp` all targets; clippy `waddle-server` lib with and without `clustering`; clippy with `-D warnings` of the five W7a test targets; `cargo check` with `test-support` of the run7 S2S and host-grant targets, the W7a targets and the local Spaces suites listed under `integration-run8-s2s.log`, and of the three clustering targets with `test-support,clustering`; the structure, typed-entry, PostgreSQL-env and authority-inventory scripts; `check-poison-recovery.py` with the same findings as `2e5563b36`; `git diff --check` from `2e5563b36`. All passed. `integration-run8-gates-rerun1.log`: the same gates at `d89c3bae8`, the fixes below included, all passed; `62af0f9d9` changes documentation only. |
| `integration-run8-s2s.log` | nextest `--profile ci` at `8d9a33400` (the W7a merge and one test fix), PostgreSQL required with a database of its own: the run7 S2S selection, the five W7a targets, `xep0503_remote_metadata`, and the local Spaces suites whose production paths W7a changed (`xep0060_space_fanout_fence`, `xep0060_spaces_owner_ws`, `xep0503_spaces_ws`, `xep0503_channel_placement`, `xep0060_pubsub_ws`, `spaces_pubsub_owner_seed`, `channel_outcast_precedence`, `admin_spaces_ws`, `admin_channel_space_link_ws`). `waddle-xmpp` 136 of 136 across 5 binaries; `waddle-server` 747 of 751 across 54 binaries, 5 skipped (the `#[ignore]` SpiceDB cases of `channel_outcast_precedence`, which run in the host-grant log). The four failures: `spaces_privileged_issuance` loaded an S2S snapshot for `example.com` under the default `.test` peer scope (test, `cf0c98c76`); `admin_channel_space_link_ws` submitted a space JID that preparation would change, which the identity profile refuses (test, `b0aa52d76`); two `xep0060_s2s_inbound_spaces_pubsub` cases, one because the fixture's in-memory PubSub store keeps a subscription for each `<subscribe/>` where the deployment's database store keeps one for an account (fixture, `72e120b68`), and one because a retract on a Space node notified nobody (product, `17eaad6f7`: XEP-0503 requires `pubsub#notify_retract` on a Space node, and XEP-0060 §7.2.2.1 then requires the notification). `integration-run8-s2s-clustering.log`: the three clustering targets, 44 of 44. |
| `integration-run8-s2s-rerun1.log` | the same selection at `62af0f9d9`, after the fixes: `waddle-xmpp` 136 of 136 across 5 binaries; `waddle-server` 751 of 751 across 54 binaries, the same 5 `#[ignore]` cases skipped. `integration-run8-s2s-clustering-rerun1.log`: 44 of 44. |
| `integration-run8-hostgrant.log` | the run7 host-grant selection (21 binaries) at `62af0f9d9`, `--run-ignored all`, PostgreSQL required, a fresh PostgreSQL-backed SpiceDB 1.56.1 on 127.0.0.1:50064 (database created and dropped by the run), `--profile ci`: 283 of 283. |
| `integration-run8-gates-rerun2.log`, `integration-run8-s2s-rerun2.log`, `integration-run8-s2s-clustering-rerun2.log`, `integration-run8-hostgrant-rerun1.log` | the gates and the three selections above at `6ae0317a2`, after the Spaces host moved its read of a subscriber's ban out of the subscription store's transaction (`0ee8556e2`) and the ban case was rewritten (`19d6a734e`): every gate passed (`check-poison-recovery.py` with the same findings as `2e5563b36`); `waddle-xmpp` 136 of 136; `waddle-server` 751 of 751, the 5 `#[ignore]` cases skipped; clustering 44 of 44; host-grant 283 of 283 with a fresh PostgreSQL-backed SpiceDB. |
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
| `s2s_codec` (54 cases) | W4a, W4c, W5a | none | Run 54/54, run8 S2S rerun2 log |
| `s2s_pre_authentication` (16) | W4c, W5a, W4d, integration | none | Run 16/16, run8 S2S rerun2 log |
| `s2s_stream_addresses` (16) | W4c, W5a, W4d, integration | none | Run 16/16, run8 S2S rerun2 log |
| `xep0178_s2s_negotiation` (47) | W4a, W4c, W4d, W5a, integration | none | Run 47/47, run8 S2S rerun2 log |
| `identity_profile` (19) | W5i | none (loopback address checks only) | Not run |
| `xep0503_remote_metadata` (3) | W7a | none | Run 3/3, run8 S2S rerun2 log |
| `xep0313_mam` (edited, 11) | W5c | none; two cases need PostgreSQL and skip without `WADDLE_TEST_POSTGRES_URL` | Not run |
| `xep0402_pep_native_bookmarks` (edited, 15) | W5i | none | Not run |
| `xep0513_mentions` (edited, 20) | W5c | none | Not run |

### waddle-server, S2S and destination routing (`server/crates/waddle-server/tests/`)

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `s2s_verifier` (162 cases in 11 modules) | W4b | none (in-memory rustls handshakes) | Run 162/162, run8 S2S rerun2 log |
| `s2s_connection_manager` (54 in 3 modules) | W4c, W4d, W6b, W6a | loopback | Run 54/54, run8 S2S rerun2 log |
| `xep0220_s2s_dialback` (39 in 4 modules) | W4d | loopback | Run 39/39, run8 S2S rerun2 log |
| `xep0368_s2s_discovery` (37 in 5 modules) | W4d | loopback; `SystemDnsLookup` cases use a loopback name server | Run 37/37, run8 S2S rerun2 log |
| `s2s_routing` (15) | W5a, integration, W6b | loopback | Run 15/15, run8 S2S rerun2 log |
| `s2s_iq_correlation` (13) | W5a | loopback | Run 13/13, run8 S2S rerun2 log |
| `s2s_reachability` (9) | W5a, W6b, W6a, W6c, W7a | loopback | Run 9/9, run8 S2S rerun2 log (W7a added one case for the Spaces classes, renamed one and extended the `Reachable` literal of another) |
| `s2s_d7_refusal` (4) | W5a, W6c (header only) | none | Run 4/4, run8 S2S rerun2 log |
| `xep0030_s2s_server_discovery` (6) | W5a, W6b | loopback | Run 6/6, run8 S2S rerun2 log |
| `xep0199_s2s_ping` (3) | W5a | loopback | Run 3/3, run8 S2S rerun2 log |
| `destination_classifier` (24) | W2, W5i, W5a | loopback | Run 24/24, run8 S2S rerun2 log |
| `xep0045_s2s_outbound_muc` (41) | W6b, integration | loopback; in-memory SQLite, and two cases on PostgreSQL that skip without `WADDLE_TEST_POSTGRES_URL` | Run 41/41, run8 S2S rerun2 log |
| `xep0410_s2s_self_ping` (5) | W6b, W6c | loopback | Run 5/5, run8 S2S rerun2 log |
| `s2s_stream_end` (12) | W6b | loopback | Run 12/12, run8 S2S rerun2 log |
| `xep0280_remote_room_carbons_ws` (7) | W6b | loopback (production `/ws` router in process) | Run 7/7, run8 S2S rerun2 log |
| `xep0045_s2s_inbound_muc` (33) | W6a, W7b | loopback (production `/ws` router, S2S router and connection manager in process); in-memory SQLite, and six cases on PostgreSQL 17 that skip without `WADDLE_TEST_POSTGRES_URL` (the managed room: a member entering, a revoked member, a room that is not federated, a remote admin's affiliation change through the store, a member refused administration, the room listed only to its member) | Not run on this tree (W7b added `a_managed_room_is_listed_only_to_a_remote_principal_its_grants_admit`, which asks again until the replica's 2 s reuse of its rooms has passed for the room closed to other servers); the other 32 ran 32/32 in the run8 S2S rerun2 log |
| `xep0045_s2s_inbound_muc_replicas` (9) | W6a | loopback; two replicas in one process, each with its own in-memory SQLite, S2S router and connection manager, and a carrier in place of the cluster's room claims and relay. Three cases hand the holder stanzas with what a dialback stream, or a public-CA certificate, would have proved. Compiled with and without `clustering` | Run 9/9, run8 S2S rerun2 log; 9/9 with `clustering`, run8 clustering rerun2 log |
| `xep0045_s2s_inbound_muc_router` (32) | W6a, W7b | loopback; the production S2S router and connection manager with a scripted room host, a seat table whose inserts a case can hold, and two remote domains (`s2s_inbound_muc_support/scripted.rs`); in-memory SQLite. Several cases depend on timers of 50 ms to 4.5 s; two bound another account's answer at 2 s behind one account's flood; one waits up to 15 s for a kept leave to be placed again, and one runs 14 s to pass a sweep of 12 s. The case for the lifecycle's stop wires the router and connection manager to a stop token as the server does. W7b's case holds the host for 1 s budgets and asserts that a room's request reaches the host within 800 ms of five room lists | Not run on this tree (W7b added `a_room_list_or_request_the_host_does_not_decide_in_time_is_still_answered` and made `a_slow_room_holds_no_stream_and_ends_no_seat` count only `resource-constraint` refusals as the immediate ones and expect every request answered); its 31 earlier cases, that one before the change, ran 31/31 in the run8 S2S rerun2 log and 31/31 with `clustering` in the run8 clustering rerun2 log |
| `xep0313_s2s_inbound_mam` (5) | W6a | loopback; in-memory SQLite, and one case on PostgreSQL 17 that skips without `WADDLE_TEST_POSTGRES_URL` | Run 5/5, run8 S2S rerun2 log |
| `xep0030_s2s_inbound_muc_discovery` (13) | W6a, W7b | loopback; in-memory SQLite | Not run on this tree (W7b replaced `disco_items_lists_neither_rooms_nor_occupants_to_remote_askers` with `the_service_lists_its_open_rooms_and_a_room_lists_no_occupants` and added `the_service_lists_no_room_a_remote_asker_may_not_reach`, `the_room_list_is_paged_for_a_remote_asker`, `the_room_list_pages_backward_and_from_an_index_for_a_remote_asker`, `an_anchor_naming_an_unlisted_room_is_answered_as_one_naming_no_room`, `a_room_list_beyond_one_answer_carries_its_set_unasked` (257 catalog rows) and `a_client_without_spaces_finds_a_spaces_room_and_enters_it_by_its_plain_jid`); the W6a cases ran 7/7 in the run8 S2S rerun2 log |
| `xep0085_s2s_inbound_chat_states` (4) | W6a | loopback; in-memory SQLite. One case waits 2 s for a rate to refill | Run 4/4, run8 S2S rerun2 log |
| `xep0115_s2s_inbound_caps` (1) | W6a | loopback; in-memory SQLite | Run 1/1, run8 S2S rerun2 log |
| `xep0308_s2s_inbound_corrections` (2) | W6a | loopback; in-memory SQLite | Run 2/2, run8 S2S rerun2 log |
| `xep0333_s2s_inbound_markers` (2) | W6a | loopback; in-memory SQLite. One case waits 2 s for a rate to refill | Run 2/2, run8 S2S rerun2 log |
| `xep0359_s2s_inbound_stanza_ids` (1) | W6a | loopback; in-memory SQLite | Run 1/1, run8 S2S rerun2 log |
| `xep0424_s2s_inbound_retraction` (2) | W6a | loopback; in-memory SQLite | Run 2/2, run8 S2S rerun2 log |
| `xep0425_s2s_inbound_moderation` (2) | W6a | loopback; in-memory SQLite | Run 2/2, run8 S2S rerun2 log |
| `xep0444_s2s_inbound_reactions` (2) | W6a | loopback; in-memory SQLite | Run 2/2, run8 S2S rerun2 log |
| `xep0461_s2s_inbound_replies` (2) | W6a | loopback; in-memory SQLite | Run 2/2, run8 S2S rerun2 log |
| `clustering_remote_room_request` (4) | W6a | none; builds with the `clustering` feature only (`--features test-support,clustering`); one case classifies the relay's typed failures through a test-support seam | Run 4/4 with `--features test-support,clustering`, run8 clustering rerun2 log |
| `s2s_account_router` (14) | W6c | loopback; in-memory SQLite | Run 14/14, run8 S2S rerun2 log |
| `s2s_account_traffic_ws` (39), the RFC 6121 account-traffic suite | W6c | loopback (production `/ws` router and the S2S router in process, a loopback peer); in-memory SQLite | Run 39/39, run8 S2S rerun2 log |
| `xep0160_s2s_account_offline_ws` (6) | W6c | as `s2s_account_traffic_ws` | Run 6/6, run8 S2S rerun2 log |
| `xep0198_s2s_account_expiry` (3) | W6c | none (in-memory offline store and registries) | Run 3/3, run8 S2S rerun2 log |
| `xep0280_s2s_account_carbons_ws` (5) | W6c | as `s2s_account_traffic_ws` | Run 5/5, run8 S2S rerun2 log |
| `xep0359_s2s_account_stanza_ids_ws` (2) | W6c | as `s2s_account_traffic_ws` | Run 2/2, run8 S2S rerun2 log |
| `xep0191_s2s_account_blocking_ws` (8) | W6c | as `s2s_account_traffic_ws` | Run 8/8, run8 S2S rerun2 log |
| `xep0060_s2s_account_events_ws` (6) | W6c | as `s2s_account_traffic_ws`, the peer also serving `pubsub.home-b.test` | Run 6/6, run8 S2S rerun2 log |
| `xep0115_s2s_account_caps_ws` (1) | W6c | as `s2s_account_traffic_ws` | Run 1/1, run8 S2S rerun2 log |
| `xep0199_s2s_account_ping_ws` (3) | W6c | as `s2s_account_traffic_ws` | Run 3/3, run8 S2S rerun2 log |
| `xep0313_s2s_account_archive_ws` (2) | W6c | as `s2s_account_traffic_ws` | Run 2/2, run8 S2S rerun2 log |
| `xep0503_s2s_inbound_spaces` (12) | W7a, W7b | loopback; in-memory SQLite; the account-traffic support serving `spaces.example.com` to the peer | Not run on this tree (W7b added `a_spaces_rooms_are_listed_in_pages`, `a_banned_account_is_refused_a_spaces_listing` and `a_spaces_listing_beyond_its_read_tells_no_count` (257 rooms and bookmarks), and a node listing to two cases); the W7a cases ran 9/9 in the run8 S2S rerun2 log |
| `xep0060_s2s_inbound_spaces_pubsub` (22) | W7a | as `xep0503_s2s_inbound_spaces` | Run 22/22, run8 S2S rerun2 log |
| `xep0503_s2s_inbound_spaces_host_grant` (4) | W7a, W7b | PostgreSQL; loopback; every case returns early without `WADDLE_TEST_POSTGRES_URL` and fails under `WADDLE_TEST_REQUIRE_POSTGRES=1` | Not run on this tree (W7b added the node listing to `a_managed_rooms_bookmark_is_told_only_to_a_principal_its_grants_admit`); ran 4/4 in the run8 S2S rerun2 log before |
| `xep0503_s2s_outbound_spaces_ws` (3) | W7a | as `s2s_account_traffic_ws`, the peer also serving `spaces.home-b.test` and `conference.home-b.test` | Run 3/3, run8 S2S rerun2 log |
| `spaces_privileged_issuance` (10) | W7a | none (in-memory SQLite, the local permission backend; one case loads an S2S snapshot through the production loader from temporary files) | Run 10/10, run8 S2S rerun2 log |
| `admin_spaces_ws` (edited, 22) | W7a | TestServer process | Run 22/22, run8 S2S rerun2 log (one case added: a privileged role refused for an account at another server) |
| `admin_channel_space_link_ws` (edited, 2) | integration | TestServer process | Run 2/2, run8 S2S rerun2 log (the escaped-projection case submits the space JID in its canonical form, which the identity profile requires of admin command fields) |
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
| `host_grant_authority` (21) | W5b, W5c, W5d, W5e | PostgreSQL 17 (superuser login; one case creates a role) | Run 21/21, run8 host-grant rerun1 log |
| `xep0045_host_grant_admission` (15) | W5b, W5c, W5e | PostgreSQL 17 | Run 15/15, run8 host-grant rerun1 log |
| `xep0045_host_grant_audience` (4) | W5b, W5c | PostgreSQL 17 | Run 4/4, run8 host-grant rerun1 log |
| `host_grant_projection` (11) | W5b, W5c, W5d | PostgreSQL 17 and SpiceDB; `live_spicedb_` cases are `#[ignore]`; one case runs replicas as separate processes | Run 11/11, run8 host-grant rerun1 log |
| `authz_schema_version` (24; 11 `#[ignore]`) | W5o, W5b | SpiceDB for the ignored cases | Run 24/24, run8 host-grant rerun1 log |
| `authz_id_codec` (12; 1 `#[ignore]`) | W1, W5o, W5i, integration | SpiceDB for the ignored case | Run 12/12, run8 host-grant rerun1 log |
| `authz_codec_incarnation` (11; 5 `#[ignore]`) | W1, W5o | SpiceDB for the ignored cases | Run 11/11, run8 host-grant rerun1 log |
| `channel_outcast_precedence` (13; 5 `#[ignore]`) | W5o, W5b | SpiceDB for the ignored cases | Run 13/13, run8 host-grant rerun1 log |
| `xep0045_host_grant_fence` (31 in 2 modules) | W5c, integration, W5d, W7b | PostgreSQL 17; loopback | Not run on this tree (W7b changed `sinks::the_enrolled_space_and_the_managed_room_are_not_described_by_the_services` and renamed it `sinks::the_enrolled_space_is_not_described_and_the_managed_room_is_listed_only_to_a_member`: the MUC service lists the managed room to a member and not to an ungranted account, and the enrolled Space node's `disco#items` is not found; it added `sinks::a_plain_spaces_listing_names_a_managed_room_only_to_a_member` and `sinks::a_managed_room_whose_grants_cannot_be_read_is_left_out_of_the_listings`); ran 29/29 in the run8 host-grant rerun1 log before |
| `host_grant_enrolled_set` (9) | W5c, integration | PostgreSQL 17 | Run 9/9, run8 host-grant rerun1 log |
| `spaces_pubsub_owner_seed` (9) | W5c, W5e | none (in-memory state) | Run 9/9, run8 host-grant rerun1 log |
| `xep0503_channel_placement` (6) | W5c | none (in-memory state, loopback) | Run 6/6, run8 host-grant rerun1 log |
| `xep0060_extension_route_room_access` (3) | W5c | none (in-memory state, loopback) | Run 3/3, run8 host-grant rerun1 log |
| `host_grant_restore` (46; 28 `#[ignore]`) | W5r, W5d, W5e | PostgreSQL 17 and SpiceDB for the ignored cases | Run 46/46, run8 host-grant rerun1 log |
| `restricted_runtime_startup` (18 in 3 modules; a fourth, `clustered`, builds with the `clustering` feature only) | W5r | PostgreSQL 17 (superuser login; the cases create roles, and one starts the server under the runtime login) | Run 18/18, run8 host-grant rerun1 log |
| `host_grant_revocation_fence` (7) | W5d | PostgreSQL 17; one case runs the `waddle-server` binary's operator subcommands | Run 7/7, run8 host-grant rerun1 log |
| `xep0045_host_grant_revocation` (14) | W5d, integration, W6a | PostgreSQL 17; loopback; one case runs a replica as a second process | Run 14/14, run8 host-grant rerun1 log |
| `account_jid_retirement` (11) | W5d, W6c | PostgreSQL 17 | Run 11/11, run8 host-grant rerun1 log |
| `xep0060_space_fanout_fence` (2) | W5d | PostgreSQL 17 (the #2056 control) | Run 2/2, run8 host-grant rerun1 log |
| `xep0410_host_grant_self_ping` (2) | W5d | PostgreSQL 17; loopback | Run 2/2, run8 host-grant rerun1 log |
| `host_grant_remote_principals` (16; 3 `#[ignore]`) | W5e, integration, W6a | PostgreSQL 17; SpiceDB for the ignored cases | Run 16/16, run8 host-grant rerun1 log |
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
| `xep0503_spaces_ws` (edited, 14) | W5i, W7a, W7b | TestServer process | Not run on this tree (W7b added `a_space_node_lists_its_rooms_and_a_client_without_spaces_enters_one`, `a_space_node_a_session_may_not_read_is_listed_as_one_that_does_not_exist` and `a_session_pages_a_space_nodes_rooms`); the other 11 ran 11/11 in the run8 S2S rerun2 log (W7a added two cases: a session's Space and channel under the startup issuance policy, and `spaces:delete` destroying only the rooms whose parent the Space is) |

### waddle-client-session

| Target | Slice | Prerequisites | Status |
| --- | --- | --- | --- |
| `tests/identity.rs` (6) | W3 | none | Not run |

### Shared support modules (not targets)

`tests/host_grant_support/mod.rs` (W5b, W5e) and its `live_spicedb.rs`,
`tests/host_grant_revocation_support/mod.rs` (W5d),
`tests/xep0060_space_fanout_fence/storage.rs` (W5d),
`tests/ws_loopback_support/mod.rs` (W5c), `tests/s2s_routing_support/mod.rs`
(W5a, W6b, W6a), `tests/s2s_remote_muc_support/mod.rs` (W6b, integration),
`tests/s2s_account_support/mod.rs` (W6c) and
`tests/s2s_inbound_muc_support/mod.rs` with its `managed.rs` and
`scripted.rs` (W6a) are compiled into the targets above and add no cases. The inbound MUC support
creates its rooms federated (its S2S snapshot sets
`[host_policy] federate_new_rooms`, and the managed fixture sets the
catalog's `federated` column); a case that needs the default clears it.
Both the inbound MUC and the account-traffic supports serve `example.com`,
outside `.test`, so their snapshots name the public peer scope. W6a changed
`s2s_routing_support` and `host_grant_revocation_support` (a replica can
run an S2S snapshot); every target that includes them ran in the run7
final logs with those changes. The submodules of `s2s_verifier`,
`s2s_connection_manager`, `xep0220_s2s_dialback`, `xep0368_s2s_discovery` and
`host_grant_fence_cases/sinks.rs` are counted under their parent targets.

W7a adds `tests/s2s_spaces_support/mod.rs`, which builds on the
account-traffic support, and changed two shared supports after the run7
logs. `s2s_account_support` gained an option (`Adjust::hosts_spaces`, off
by default) that adds `spaces.example.com` to the local deployment's S2S
domains, has the peer admit and reach it, and attaches the production
Spaces host; with the option off the fixture builds what it built before.
`s2s_inbound_muc_support` gained a sender for another S2S domain of the
deployment (`Hosted::peer_sends_to`). Every target that includes either
support compiles with the change, and every one of them ran in the run8
S2S rerun1 log. From `72e120b68` the account-traffic support serves a
deployment that hosts its Spaces from the database-backed PubSub store a
deployment runs (`websocket_state_with_federation_and_database_pubsub`);
without `hosts_spaces` it builds what it built before.

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
| `waddle-server/src/admin/space_catalog_cascade_tests.rs` | W7a | none; one call of `destroy_space_channel` gained the `SpaceService` argument (which now names the account domain, as `run_delete` does: `"localhost"` in the four calls), and `run_delete` there now also closes the Space to other servers, removes the service's subscription records of accounts at other servers and the Space's own relations, runs under the Space's deletion lock, and destroys a room only where its parent grant on the Space is found, which the cases' `link` writes | Not run; not compiled |
| `waddle-server/src/server/xmpp_channels.rs` (`tests`) | W6a | none; the catalog row literal gained the `federated` column | Not run |
| `waddle-xmpp/src/muc/owner/tests.rs` | W6a | none; two cases added (the `federated` field of the owner form, and a stored configuration without it) | Not run |
| `waddle-server/src/admin/space_catalog_cascade_tests.rs` | W5c | none | Not run |
| `waddle-server/src/auth/directory/tests.rs` | W5i | none | Not run |
| `waddle-server/src/auth/native/tests.rs` | W5i | none | Not run |
| `waddle-server/src/db/migrations/tests.rs` | W5b | PostgreSQL for the `postgres_` cases; they skip without `WADDLE_TEST_POSTGRES_URL` | Not run |
| `waddle-server/src/db/migrations/after_baseline.rs` (its `tests` module: migrations 2010 and 2011 in the version order) | W6c | none | Not run |
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
| Test targets listed | 120 (97 integration targets, 21 library test modules, 2 chat test files) |
| Run, with a cited log | 68 targets, 1,005 cases, all passed in their latest cited run |
| Not run | 52 targets (21 of them library modules) |

W7b (the MUC fallback) changed seven targets that had run, which are
listed as not run on this tree: `xep0030_s2s_inbound_muc_discovery`,
`xep0045_s2s_inbound_muc`, `xep0045_s2s_inbound_muc_router`,
`xep0503_s2s_inbound_spaces`, `xep0503_s2s_inbound_spaces_host_grant`,
`xep0503_spaces_ws` and `xep0045_host_grant_fence`; it added no target. It added
`Hosted::catalogued_room` to `s2s_inbound_muc_support`, which every target
that includes the support compiles with. It changed the production paths
of the MUC service's `disco#items` for sessions (a managed room is listed
to an account whose grants allow `view`) and of a Space node's
`disco#items` for sessions (the Space's rooms are listed where an empty
list was answered), so the targets that exercise them should run again in
verification although their cases did not change: `admin_channels_ws`,
`group_dm_ws`, `xep0030_discovery_destination_ws`, the
`websocket::tests::muc` and `websocket::tests::iq` library modules, and the
local Spaces suites of the run8 S2S log. It also changed the router's
answer to any request the room host did not decide within the per-item
budget (answered as its kind is, where an IQ or a message was dropped),
and moved the service's room list off the room shards, so
`xep0045_s2s_inbound_muc_replicas` and `clustering_remote_room_request`
should run again with `clustering` too.

W7a added six targets, listed `admin_spaces_ws` for the case it added
there, added two cases to `xep0503_spaces_ws` and one to `s2s_reachability`.
It also renamed the send helpers of `s2s_account_support` and
`s2s_inbound_muc_support` (`from_peer` became `peer_sends`,
`from_peer_service` `peer_service_sends` and `from_sender` `send_from`) and
named a type in the scripted room host, so that clippy accepts the 27
targets that include either support; the call sites changed by name only.
Every one of those targets ran in the run8 S2S rerun2 log, and the
integration branch lists `admin_channel_space_link_ws` for the case it
fixed there.

By log: the run8 S2S rerun2 log covers 59 targets (136 `waddle-xmpp` cases
in 5, 751 `waddle-server` cases in 54, the 5 `#[ignore]` SpiceDB cases of
`channel_outcast_precedence` skipped there and run in the host-grant log);
the run8 clustering rerun2 log covers the 3 clustering targets (44 cases);
the run8 host-grant rerun1 log covers the 21 host-grant targets (283 cases). Before
them, the run7 S2S final log covered 44 targets (133 `waddle-xmpp` cases
in 4, 621 `waddle-server` cases in 40); the run7 clustering final log
covers 3 targets with the `clustering` feature (44 cases; two of them also
in the S2S log); the run7 host-grant final log covers 21 targets (283
cases). Every target of W6a and W6c ran there for the first time.

A target is counted as run only when the whole binary's cases appear in a
log. Skipped prerequisites are not recorded as passed: nothing here was run
against Prosody, ejabberd or a second deployment.

The runs found test defects and one product defect, each fixed in a
commit of its own and listed in the run7 rows above; no assertion was
removed. Three expectations changed for a reason outside the test: the
members-only join (XEP-0045 §7.2.6), the moderator refusal's text after
trusted homes may hold the role (ADR-0020 D8), and the startup roster
prune, which keeps remote contacts (G18-10). The product defect: on a
deployment without a canonical grant store, decisions that name a seated
remote occupant read no seat evidence, so an owner or admin standing was
issued for an account seated on a dialback stream.

What the runs do not show: the inbound MUC cases with two replicas run
both in one process and hand a carried request from one router to the
other directly, so the cluster's room claims and the relay hop of
`waddle.clustering.relay.remote_room_request.v1` are compiled and not
exercised by any case, and need two processes with the `clustering`
feature (verification steps 6 and 8), the cluster's carrier
(`ClusterRoomCarrier`) included. The cases for what a weaker stream proves
use carried stanzas, not a second kind of stream. The kept leaves'
first-kept deadline, their per-account bound and the carrier's rule for
an ask begun with less than the owner's windows have no case of their
own.

W7a's targets ran in the run8 logs. Before they did, the slice gates
compiled them and ran clippy with `-D warnings` over the 27 targets that
include the S2S account or inbound MUC support, `spaces_privileged_issuance`,
`xep0060_spaces_owner_ws` and `admin_spaces_ws`. Before the helper rename
above, clippy refused those 27 targets (`wrong_self_convention` on the
`from_*` helpers, `type_complexity` in the scripted room host), so an
earlier statement that clippy had passed on each W7a target was wrong.

What the W7a targets do not cover: a stream of `dialback` strength (the
fixtures' peer authenticates by certificate, so a dialback subscription is
recorded through the store, and "not honoured" for a request is shown
through the trusted-home list alone); concurrency on PostgreSQL (the
advisory locks that make the bounds exact and close the race between a
subscription and turning the setting off run only on the SQLite fixture,
where one immediate transaction serialises every writer; on PostgreSQL the
host-grant Spaces suite shows a sweep of the whole store passing over a
row another transaction holds, and a Space's deletion lock holding off a
second deletion of it, and no race between two writers); the router's
per-pair share of queued notifications, a Space's allowance of notified
publications and the queue of publications, which depend on timing; a
request the Spaces host does not decide in time; two replicas serving one
Space; and every third-party flow of the plan's interoperability matrix (a
Space on stock Prosody or ejabberd PubSub, and a Prosody or ejabberd
account reading a Waddle Space), which need the interoperability harness.

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

- MUC fallback (W7b), written and not run: the MUC service's
  `disco#items` lists to remote askers the rooms that are public, open to
  other servers and no group conversation, and a managed room only to a
  principal its grants admit to view it; a Space node's `disco#items` lists
  the rooms the Space holds that the asker may be told of; listings page
  with XEP-0059. Left open: `http://jabber.org/protocol/rsm` and a room's
  parent Space are not told to remote askers in `disco#info` (gate G21); a
  remote listing reads at most 4,096 catalog rows and a Space's at most 256
  items; a listing's grant decisions are taken before the answer is sent
  and not asked again after (as for the other room and Space listings).
- Spaces (W7a), written and not run: the Spaces service is reached for the
  operations reviewed for a remote principal, a Space only while its
  `federated` setting is on, and event notifications leave for the
  subscriptions remote accounts made themselves. Left open: serving an
  enrolled scope's Space (it needs a PubSub admission identity), placing a
  room in a Space from another server (no writer issues the Space `owner`
  relationship to a remote account), the `authorize` access model, ending
  a subscription on a delivery error (a subscription lapses after thirty
  days without being made again), a `federated` setting for single peers,
  the field in the web client, and advertising the further PubSub features
  (W11). A trusted home's owner holds every owner power over a Space,
  deletion included; a deletion through PubSub, by a session or a remote
  owner, takes the Space's rooms out of it and destroys none, and only the
  admin command `spaces:delete` destroys rooms. The recovery path is
  `spaces:set-role`.
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
  stay until their stream is lost or the replica restarts; the
  per-account share of the request rate and the per-account archive
  allowance are kept in memory on one replica; a room's stanzas carry no
  occupancy generation, so a presence update of an earlier occupancy that
  is delivered again reaches the seat of an occupant that entered again
  (presence that ends an occupancy is checked against the room, and a
  message is told by its archive identifier); requests for one room are
  placed one at a time, so a room whose holder does not answer delays that
  room's other requests by up to the placement timeout; an owner or admin
  standing is refused while a seat of the account would not honour it,
  where holding back the role of single seats would need the room actor
  to derive a role for each seat; a room without a catalog row that an
  owner opened by hand while the deployment default is off is reached only
  through the replica that holds it; MAM is answered and not advertised to
  remote askers; the web client does not offer the `federated` field.
- Outbound MUC (W6b), merged: a stream that waited for the router and ends
  for a deadline, eviction or replacement before reading again is reported
  as lost even when the peer wrote nothing more; end of file between
  elements, including a TLS end without `close_notify`, is closed, so a
  peer that crashed between stanzas looks like one dropping an idle
  connection and its occupants are not told.
- Account traffic (W6c), done on its branch for the handlers it reviewed:
  both D7 refusals replaced once the account side is attached, inbound
  requests registered for correlation, the DM pin marker refused by the
  router, and the sender's blocking list checked before messages and
  presence are offered. The ingress receipt for a remote target remains
  (see the account-traffic design below).
- Effect-boundary admission (W5e): take the `RemotePrincipal` at the
  protected-effect boundary.
- Capability advertisement (W11): any further feature in server-domain
  `disco#info` for remote askers.
- Stanzas the interpreter routes to a remote JID other than IQs, such as a
  MUC mediated invitation to a remote account, are still dropped silently by
  local resource selection.
- Router metrics through the telemetry macros.

### Account-traffic design (`s2s-account-traffic-2047.md`)

- A remote ingress producer, shared with inbound MUC (W6a, whose branch
  adds `IngressPrincipal::Remote`): remote senders' messages are stored
  offline today as `Transient` rows outside the ingress commit, without
  the recipient's archive, inbox or push notification; at the merge the
  account side's producer moves to the ingress path, keeping the
  strangers' bounds, the silent drop and the no-bounce guard
  (`sm_promotion::is_remote_sender`). The sender's archive and inbox for
  local accounts' messages to remote contacts, which reach the sender's
  other devices only as carbons, wait for the same producer.
- Remote access to vCard-temp, PEP items, account discovery and last
  activity at an account's bare JID, each with its own review; PEP
  notifications from local accounts to remote contacts.
- Directed presence tracking for remote entities.
- Presence at login to more than about 16 remote contacts with `both`
  (32 with `from`) on one domain can find the account's share of the
  pair's queue full and drops the excess.
- The account lock that serialises roster read-modify-write for remote
  contacts, the client's roster set included, is per replica; no case
  forces an inbound subscription stanza to interleave with a roster set,
  so the lock in the roster set is reviewed, not exercised.
- A stream left open at retirement is a host-grant design question; the
  router refuses everything it sends to remote JIDs, but the departure
  presence of a resource that ends still leaves its remote rooms.
- A detached session's queue has no bound per sender: the strangers'
  bound of 512 per account and day keeps them below the queue's 1,000,
  while a contact's stanzas are bounded by that queue only.
- The admission of the sender's domain is checked again before an offline
  flush only while S2S is enabled; messages stored while it was on are
  delivered after it is turned off.
- No case drives a store failure (the no-announcement rule of a failed
  roster write, the failed request lookup).

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
  reader); a per-scope `require_pkix`; an operation kind of its own for a
  dropped increase, which needs a migration of the kind constraint; and the
  rollout window in which a replica on an earlier snapshot can still
  activate an increase another replica would drop. W7a decides D8 issuance
  for the Spaces relations at their writers, and for the channel and Space
  `owner` and `admin` relations at the permission actor as well. Not covered by a test: the call mixer's domain in the
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
