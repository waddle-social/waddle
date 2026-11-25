# XEP-0198 stream management verification

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

## Verify the Rust implementation

The `verifyStreamManagementRust` CI job checks the production Rust counter
and lifecycle modules with [Kani](https://model-checking.github.io/kani/).
The isolated proof crate compiles those source files directly. The harnesses
check:

- Every ordered window whose last ordinal fits in a signed 64-bit database
  column and whose retained span is below half the wire counter space.
  For every 32-bit `h`, the implementation agrees with the absolute-ordinal
  contract for live and resume acknowledgments. A successful acknowledgment
  updates only the acknowledged floor. Replay starts above that floor and
  ends at the unchanged appended ordinal.
- For the same windows and every 32-bit sequence, the replay lookup returns
  the matching retained ordinal exactly when it exists.
- Across the lifecycle state/event pairs, terminal states reject all events,
  resume enters hydration only from an eligible state, publication requires
  hydration, and expiry requires a detached session.

Run the same harnesses locally with Kani 0.68.0 installed:

```sh
cd server/specs/stream-management/proof
cargo fetch --locked
CARGO_NET_OFFLINE=true cargo kani
```

The counter harness inputs are symbolic across their full stated domains.
The lifecycle contract concerns the pure transition function. The proofs
do not establish PostgreSQL claim atomicity, orchestration correctness, or
delivery of replay bytes. The Quint model explores abstract handoff
interleavings independently of the Rust proofs.
