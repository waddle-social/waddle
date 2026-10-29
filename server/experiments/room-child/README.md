# Accepted room-child PostgreSQL characterization

From `server/`, run `bash experiments/room-child/run-native.sh`. The runner enters
an isolated native Nix shell using the repository's `flake.lock` and Rust toolchain,
requires PostgreSQL 17.11, checks the exact five-test inventory, and requires five
passed tests with zero failures or ignored tests. The tests are deliberately
ignored by general unit-test invocations; the dedicated non-publishing CI lane
runs this command. Missing PostgreSQL configuration fails each selected test.

The runner owns a temporary PostgreSQL cluster and database, reachable only via a
private Unix socket under a mode-0700 temporary directory. Local trust auth avoids
password material; TCP listening is disabled and host authentication is rejected.
PostgreSQL startup/shutdown are bounded at 30 seconds, initialization at 60 seconds,
compilation at 30 minutes and the test run at 3 minutes (with 10 seconds
termination grace). Cargo uses two build jobs, no debug information and no
incremental compilation; an optional `CARGO_TARGET_DIR` survives the Nix shell
for local artifact reuse. Cleanup uses coordinated
`pg_ctl -m fast` shutdown, including PostgreSQL's independently grouped backends.
If shutdown cannot be confirmed, the run fails and retains the directory for
inspection; it never deletes live or uncertain PGDATA or uses a group-kill fallback.
Uncatchable process/host termination requires external cleanup of that private root.

The cases all use PostgreSQL admission storage:

- `postgres_sealed_child_delayed_release_preserves_payload_and_target`: real room
  planning and sealed commit, then a newcomer and a config revision before delayed
  maintenance. Original recipient and sender reflection receive the frozen content
  exactly once in persisted SM custody; the newcomer receives nothing.
- `postgres_sealed_child_leave_rejoin_retires_original_without_delivery`: a fresh
  occupancy at the same full JID cannot inherit the original accepted child.
- `postgres_sealed_children_destroy_recreate_retire_without_transport`: both absent
  and recreated single-node rooms retire original children with explicit policy
  receipts and no SM custody, including when the same JIDs join the replacement.
- `postgres_sealed_child_rejects_changed_payload_or_target_before_sink`: the real
  persisted-authority loader accepts original route/reflection children and rejects
  changed bodies or a resource outside the manifest without recording progress.
- `postgres_sealed_manifest_preserves_children_after_durable_revision_advance`:
  a minimal durable-coordinate admission fixture rejects stale new acceptance;
  replay of an accepted manifest preserves the original children after revision
  advancement. This case does not model a complete durable room store.

These are characterizations of existing behavior, not a change in authorization.
They use production planning, sealing and recovery without a legacy roster fixture.
The four actor cases exercise single-node authority; neither unknown clustered
occupancy nor a lost clustered lifecycle is inferred to be retired. They do not
prove clustered handoff, socket transport, process-crash recovery, concurrent
revocation, grant authority selection, or federation activation. SM assertions
establish persisted replay custody, not delivery to a remote client.
