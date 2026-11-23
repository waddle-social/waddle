# Detached stream resume

This Quint model starts with one durably detached XEP-0198 stream. Two nodes
can each attempt resume once. At most three stanza ordinals exist. The
initial window contains two retained frames, only one of which has been
exposed to the client. A later frame can arrive before or during hydration.

## Run the checks

From the repository root, with Bun and Java 21 available:

```sh
bunx @informalsystems/quint@0.34.0 test --backend=typescript server/specs/stream-management/detached_resume_test.qnt
bunx @informalsystems/quint@0.34.0 verify server/specs/stream-management/detached_resume.qnt --invariant=safety --backend=tlc
```

The named runs cover stale claims and witnesses, successive generations,
receipt abandonment, acknowledgment beyond exposure, expiry, owner loss,
and the replay tail. TLC explores all reachable states of this finite model, including other
orderings of those actions.

## Correspondence to Rust

| Model action | Implementation boundary |
| --- | --- |
| `acquire` | Successful claim-store CAS. The ticket represents the exact claim fence. Its election, lease renewal, and failed CAS outcomes are outside the model. |
| `commit` | [DurableSmStore::resume](../../crates/waddle-server/src/sm_authority/resume.rs) validates the claim and handled ordinal, advances the generation, and persists the operation receipt in one transaction. |
| `observe`, `abandon` | The caller obtains or recovers a witness for its committed operation, or durably abandons an operation that did not commit. The model uses a single observation for the original commit witness and a recovered witness. The former captures replay at commit, while receipt resolution can take a fresh snapshot. An old witness can remain in caller memory after takeover. |
| `admitFrame`, `publish` | The witness replay precedes the hydration tail. Fenced completion exposes the retained range before publication. The model records this as one observation of successful completion, including the ordered replay plan. |
| `deadline`, `ownerLost`, `expire` | Time can elapse while detached or hydrating. A hydration that loses ownership can become detached and eligible for terminal expiry. The expiry action represents a reaper whose own claim check succeeded. |

`safety` checks that durable resume commits and publications use the current
claim, committed generations are unique, publications advance by generation,
and replay is exactly the ordered suffix above the acknowledged frontier,
including any hydration tail. Two attempts can both succeed if they publish
different generations. The
[cross-node integration suite](../../crates/waddle-server/tests/xep0198_durable_cross_node_resume.rs)
checks the settled live owner and real replay behavior.

The model assumes a valid principal and successful atomic storage operations,
with receipt observation allowed to lag behind commit. It uses small absolute
ordinals and abstracts the resume deadline as a boolean. It excludes wire counter
wraparound, XML and bytes, transport writes, live admissions after publication,
database failure semantics, races to acquire the reaper claim, and liveness. The finite bounds apply to the model.

## Replay a trace against Rust

The hydration-tail scenario exports an ITF trace that the opt-in
[authority test](../../crates/waddle-server/tests/sm_durable_resume.rs) replays
against the production durable store with SQLite. The test maps the modeled
resume to a fresh binding, then compares the replay snapshot, ordered
completion tail, and final persisted window with the exported trace.

```sh
trace_dir=$(mktemp -d)
bunx @informalsystems/quint@0.34.0 test --backend=typescript \
  --match=replayIncludesHydrationTailTest \
  --out-itf="$trace_dir/{test}_{seq}.itf.json" \
  server/specs/stream-management/detached_resume_test.qnt
(
  cd server
  WADDLE_QUINT_RESUME_TRACE="$trace_dir/replayIncludesHydrationTailTest_0.itf.json" \
    cargo test --locked -p waddle-server --test sm_durable_resume \
    replay_quint_hydration_trace -- --ignored --exact
)
```

The trace adapter covers this one scenario. The SQLite test uses a local
binding, so the modeled claim acquisition has no corresponding store call.
The native PostgreSQL suite linked above exercises real cross-node claims
and XMPP wire behavior independently.
