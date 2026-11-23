# Detached stream resume

This Quint model starts with one durably detached XEP-0198 stream. Two nodes
can each attempt resume once. At most three stanza ordinals exist. The
initial window contains two retained frames, only one of which has been
exposed to the client. A later frame can arrive before or during hydration.

## Run the checks

From the repository root, with Bun and Java 21 available:

```sh
bunx @informalsystems/quint@0.34.0 test --backend=typescript server/specs/stream-management/detached_resume_test.qnt
bunx @informalsystems/quint@0.34.0 verify server/specs/stream-management/detached_resume.qnt --invariant=safety --max-steps=12
```

The named runs cover stale claims and witnesses, successive generations,
receipt abandonment, acknowledgment beyond exposure, expiry, owner loss,
and the replay tail. The bounded check explores other orderings of those
actions, including stuttering.

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
database failure semantics, races to acquire the reaper claim, and liveness. The bound applies to the model. The
Rust integration suite remains the implementation check, and these Quint runs
do not drive Rust.
