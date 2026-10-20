# Filesystem Hierarchy Cleanup Plan

Status: proposed. Planning only; this document does not change any code.

## Premise check

The working assumption is that an underscore in a filename means the code needs a
directory. That is true for some files and not for others.

Of the 4,535 tracked files, 1,113 have an underscore in the basename. Most of
those underscores are required by the language or toolchain, or are just a
multi-word name. Moving those files would create churn and could break tooling.

| Bucket | Approx. files | Verdict |
| --- | --- | --- |
| Rust files under `server/crates` | 913 | Mixed; see the rule below |
| Android Paparazzi screenshots (`*_dark.png`, `*_light.png`) and `res/` | 81 | Leave. Resource names must be `[a-z0-9_]` |
| Go `*_test.go` and `cmd/`, `internal/*` files | 71 | Mostly leave. A Go directory is a package, so splitting one requires exporting symbols |
| Python `server/scripts/test_*.py` | 16 | Move. They sit beside the scripts they test |
| Drizzle migrations and `meta/_journal.json` | 15 | Leave. drizzle-kit generates them |
| Generated UniFFI and wasm-bindgen bindings | 6 | Leave. Generated |
| `apps/apple/ci_scripts/ci_post_clone.sh` | 1 | Leave. Xcode Cloud requires this exact path |
| Helm `_helpers.tpl`, `_cutover-guard.tpl`, Cloudflare `_headers` | 4 | Leave. These names are tool conventions |
| `server/vendor/kameo` | 2 | Leave. Vendored code |

In Rust, snake_case module names are idiomatic. `rich_target_validation.rs` is
one concept, not a flattened hierarchy. The real signal is narrower.

### Rule: when an underscore is a hierarchy smell

A file is a candidate to move only if at least one of these holds:

1. **Shared prefix.** At least three siblings in the same directory share the
   first `_`-delimited segment. Example: 41 `ingress/execute_*.rs` files.
2. **Satellite file.** `foo_bar.rs` sits beside `foo.rs` or `foo/`. Example:
   `server/xmpp_state.rs` with `xmpp_state_tests.rs`.
3. **`#[path]` wiring.** The module is attached with a `#[path = "..."]`
   attribute. There are 201 of these outside `vendor/`: 102 point at test
   files and about 99 at sibling implementation files. Each one fakes a parent
   module that should be a directory.

Anything else stays as it is.

## Findings by area

### `waddle-server/src/ingress/` (125 entries, flat)

| Prefix | Files |
| --- | --- |
| `execute_*` | 41 |
| `recovery_*` | 19 |
| `room_*` | 5 |
| `maintenance_*` | 5 |
| `append_authority_*` | 5 |
| `commit_*`, `live_delivery_*` | 3 each |

Test files also sit loose next to the code they test, for example
`alias_denial_replay_tests.rs` and `offline_hardening_tests.rs`.

### `waddle-server/src/` (top-level modules)

These modules share a domain prefix:

- `ingress`, `ingress_substrate`, `ingress_uow`
- `notification_activity`, `notification_outbox`, `notification_settings_projection`
- `sm_persistence`, `sm_persistence_fenced`, `sm_promotion`
- `push_registrations`, `push_service`
- `room_effect_outbox`, `room_observation`, `room_policy`
- `dnd_projection`, `dnd_reader`
- `pubsub`, `pubsub_authz`
- `spaces_metadata`, `spaces_pubsub_seed`, `space_identity`
- `muc_durable`, `muc_destroy_completion_outbox`

### `waddle-server/src/server/`

- 12 `xmpp_*_state.rs` and `xmpp_*.rs` files.
- `extension_bot*`, `extension_host_*` and `extension_commands/`.
- `state_inventory*`.
- `routes/interpret/`: `room_*` (7), `direct_*` (6), `groupchat_*` (4).
- `routes/websocket/handlers/iq/`: `muc_*` (7), `spaces_*` (5),
  `community_*` (4), `link_*` (3).

### Other crates

- `waddle-xmpp-client-wasm/src/`: 15 `client_*.rs` files.
- `waddle-xmpp/src/muc/`: `room_*`.
- `waddle-extensions/src/manager/`: `message_*`.

### Integration tests (`waddle-server/tests/`, 117 entries)

Every top-level `.rs` file in `tests/` compiles into its own test binary, and
each binary links the whole server. Grouping test files here also reduces link
time, not just naming. Families:

| Family | Files |
| --- | --- |
| `xep0045_*` | 10, plus 3 directories |
| `xep0198_*` | 9 |
| `xep0357_*` | 7 |
| `admin_*` | 5 |
| `ingress_*` | 4 |
| `xep0313_*`, `xep0359_*`, `xep0060_*`, `rfc6121_*`, `proto_calendar_*`, `push_service_*` | 3 each |

There are also 10 `*_support` files or directories that serve as shared
helpers.

### Module style is mixed

There are 133 self-named modules (`foo.rs` plus `foo/`) and 76 `mod.rs`
modules. Self-named is the majority, so standardise on it.

### Very large files (code cleanup)

| File | Lines |
| --- | --- |
| `server/session_janitors.rs` | 15,335 |
| `routes/websocket/tests/stream_management.rs` | 14,176 |
| `muc/room_registry_actor/tests.rs` | 12,285 |
| `muc/room_actor/tests.rs` | 11,401 |
| `admin/channels.rs` | 9,618 |
| `muc_durable.rs` | 8,167 |

## Hidden coupling: renames that fail silently

Module paths and binary names are strings in configuration. Renaming a module
does not cause a compile error when one of these strings stops matching.

- `server/.config/nextest.toml` has 21 `binary(...)` and 27 `test(...)`
  filters. They put Postgres-backed tests in the single-threaded `postgres`
  group and reserve the whole machine for the clustering end-to-end binaries.
  Moving `ingress_substrate` to `ingress::substrate` leaves
  `test(ingress_substrate::)` matching nothing. Those tests then run in
  parallel against the shared database, and the result is flaky CI with no
  error that points at the cause.
- These files pass `--test <binary>` names: `server/Justfile`, `flake.nix`
  (`xmpp_e2e_cue`, `jev_guest_runtime`) and `server/AGENTS.md`.
- `ci/rust-tests/workflow.cue` calls `server/scripts/test_*.py` by path. The
  workflow YAML is generated from it.
- `plan_nextest_binary_shards.py` balances CI shards by binary ELF size, so
  merging test binaries changes the shard plan.

## Phases

Each phase is one or more PRs. Every move PR is mechanical: `git mv` plus
`mod`/`use` and visibility edits, with no logic changes, so that
`git log --follow` and review stay usable. Run the phases sequentially within a
crate, because parallel move PRs conflict.

### Phase 0: guardrails (lands first)

1. **Nextest filter liveness check.** Add a script, run in CI, that fails when
   any `binary(...)` or `test(...)` filter in `nextest.toml` matches zero tests
   in `cargo nextest list`. Without it, every later phase is a risk.
2. **Test-count baseline.** Each move PR must show the same per-crate test count
   in `cargo nextest list` before and after. Test names change; counts do not.
3. **Detector script.** Add `scripts/check-flat-modules`, which reports Rust
   files that match the three rules above. It is advisory until Phase 6.

### Phase 1: remove `#[path]` (201 sites)

- Test satellites: `foo_tests.rs` attached with
  `#[path = "foo_tests.rs"] mod tests;` moves to `foo/tests.rs` and a plain
  `mod tests;`. Create `foo/` beside `foo.rs` where needed.
- Implementation satellites, such as `ingress_authority_matrix.rs` and
  `registration_owner_local.rs`, become real child modules of their parent.
- Exit criterion: zero `#[path]` outside `vendor/`, enforced by a CI grep.

This phase resolves rule 3 across the tree and most of rule 2 before any
domain-level restructuring.

### Phase 2: `waddle-server/src/ingress/`

Target layout:

```
ingress.rs
ingress/
  execute.rs      execute/{archive,carbon_progress,dependencies,detached,invite,muc_fanout,observers,offline,projection,recovery,relay_copy,uow,...}.rs + execute/tests/
  recovery.rs     recovery/{departed,environment,executor,ghosts,invitation,muc,reachability,rebuild,...}.rs + recovery/tests/
  maintenance.rs  maintenance/{stalls,waits}.rs + tests
  append_authority.rs  append_authority/{carbons}.rs + tests
  commit.rs       commit/{room,stream}.rs (merge with existing commit/)
  live_delivery.rs     live_delivery/{test_hooks}.rs + tests
  room/           {canonical,manifest,manifest_replay}.rs
  receipts.rs     receipts/{routing}.rs
  ...existing archive_authority/, recorded/, restamp/, suppression/
```

The main code cost is visibility. Items that are `pub(super)` today need
`pub(in crate::ingress)`. Do not widen anything to `pub(crate)` just to make the
build compile.

### Phase 3: `waddle-server/src/` top-level domains

| From | To |
| --- | --- |
| `ingress_substrate`, `ingress_uow` | `ingress::substrate`, `ingress::uow` |
| `notification_{activity,outbox,settings_projection}` | `notification::{activity,outbox,settings_projection}` |
| `sm_persistence`, `sm_persistence_fenced`, `sm_promotion` | `stream_management::{persistence, persistence::fenced, promotion}` |
| `push_registrations`, `push_service` | `push::{registrations,service}` |
| `room_{effect_outbox,observation,policy}` | `room::{effect_outbox,observation,policy}` |
| `dnd_projection`, `dnd_reader` | `dnd::{projection,reader}` |
| `pubsub_authz` | `pubsub::authz` |
| `spaces_metadata`, `spaces_pubsub_seed`, `space_identity` | `spaces::{metadata,pubsub_seed,identity}` |
| `muc_durable`, `muc_destroy_completion_outbox` | `muc::{durable,destroy_completion_outbox}` |

Single-member modules stay where they are: `call_teardown_outbox`,
`channel_space_links`, `pending_delivery`, `occupancy_authority`,
`pep_feed_bridge` and `test_support`.

Update the `test(...)` filters in `nextest.toml` in the same PR. The Phase 0
check proves the update is complete.

### Phase 4: `waddle-server/src/server/`

| From | To |
| --- | --- |
| `xmpp_{account,app,auth,permission,profile,roster,space,upload,user_storage}_state.rs` | `xmpp_state/{account,app,...}.rs` |
| `extension_bot{,_avatar,_rooms}`, `extension_host_{adapter,tools}`, `extension_commands/` | `extension/{bot, bot/avatar, bot/rooms, host_adapter, host_tools, commands}` |
| `state_inventory{,_metrics,_route}` | `state_inventory/{metrics,route}` |
| `routes/interpret/` `room_*`, `direct_*`, `groupchat_*` | `room/`, `direct/`, `groupchat/` |
| `routes/websocket/handlers/iq/` `muc_*`, `spaces_*`, `community_*`, `link_*` | `muc/`, `spaces/`, `community/`, `link/` |

### Phase 5: other crates

Run the detector on the remaining crates and fix what it reports:

- `waddle-xmpp-client-wasm`: `client_*.rs` moves to `client/`.
- `waddle-xmpp`: `muc/room_*`.
- `waddle-extensions`: `manager/message_*`.
- `waddle-xmpp-client`.

### Phase 6: integration tests

- **Recommended:** turn each multi-file family into one binary per family, for
  example `tests/xep0045/main.rs` with the current files as modules, and
  likewise for `xep0198`, `xep0357`, `admin`, `ingress`, `push_service`,
  `rfc6121` and `proto_calendar`. Move the `*_support` helpers into a shared
  `tests/support/` module. This cuts roughly 117 binaries to about 70.
- **Rejected for now:** one `tests/it/main.rs` per crate. It links fastest, but
  it defeats ELF-size binary sharding. It also removes the per-binary
  `threads-required = 'num-cpus'` isolation that the clustering end-to-end tests
  depend on (#1627).
- Leave `waddle-xmpp/tests/xepNNNN_name.rs` single files as they are. The XEP
  number is the identifier. Only group `xep0198` (4), `xep0045` (3) and
  `ingress` (3).
- In the same PR, update `nextest.toml` `binary(...)` filters, `server/Justfile`,
  `flake.nix`, `server/AGENTS.md` and the shard plan.

### Phase 7: non-Rust (small)

- Move `server/scripts/test_*.py` to `server/scripts/tests/`, and the nextest
  shard tooling to `server/scripts/nextest/`. Update
  `ci/rust-tests/workflow.cue` and regenerate the workflows with cuenv.
- Merge `infrastructure/waddle.cloud/internal/secrets/onepassword_*` (5 files)
  into an `internal/secrets/onepassword` package. That boundary is real.
- Leave Go `talos/client_*`, `scaleway/provision_*` and `cmd/cluster_*` as they
  are. They are methods on one type or one package, and prefixed filenames are
  idiomatic Go for that.

### Phase 8: code cleanup and enforcement

- Split the files over 8,000 lines along their existing `impl` and section
  seams, now that each has a directory to grow into. Do this in separate PRs
  from the moves, and only where the result has a meaningful boundary.
- Set `clippy::mod_module_files = "deny"` at the workspace level after
  converting the 76 `mod.rs` modules. Do the conversion one crate at a time.
- Make the detector script a hard CI failure, with an explicit allowlist.

## Verification per PR

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets --all-features -D warnings`
- The `cargo nextest list` test count is unchanged per crate.
- The nextest filter liveness check passes.
- The full CI run is green.

## Decisions needed

1. **Module style.** Self-named `foo.rs` plus `foo/` (recommended; 133 vs 76),
   or `mod.rs`.
2. **Integration test grouping.** Per-family binaries (recommended) or one
   binary per crate.
3. **Renames during moves.** Phase 3 expands `sm_` to `stream_management`. Allow
   name changes in move PRs, or keep moves strictly path-only and rename
   separately.
