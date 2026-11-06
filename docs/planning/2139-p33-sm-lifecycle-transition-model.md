# P3.3 dark SM lifecycle transition model

Status: implemented as an inactive fixture for #2139 under #2046. This is a deterministic, in-memory test/test-utils model; it does not add production behavior.

## Model

The lifecycle states are EnablingLive, Live, DetachedResumable, TransferHydrating, and Terminal. Identity checks use the exact BindingRef and ConnectionRef values. A distinct checked, nonwrapping session-generation value fences durable-session identity; physical transport generations and process-local ordinals are not session generations.

Enabling requires a modeled durable-commit receipt before the enabled effect is available. Resume is planned as one atomic compare-and-set over the expected authenticated principal, session generation, claim, binding, ACK, and endpoint/resource installation state. Only the exact publication and hydration receipt completes TransferHydrating and permits replay or ingress. A losing, stale, mismatched, or revoked owner does not mutate state.

Terminal evidence is retained as tombstone/repair custody. Late cleanup can only apply to its exact binding and generation. Unknown commit and uncertain frame-write outcomes retain repair responsibility.

## Frontier

The frontier maintains nonwrapping append, exposure, peer-ACK, and contiguous-ingress ordinals with peer_acked <= exposed <= appended. New exposure is recorded immediately before its corresponding attempt. Retry and replay include all retained rows above peer_acked, including rows never exposed. Wire h values map only within the 32-bit half-space and the model rejects out-of-ceiling ACKs before mutation.

Existing replay behavior adds a delay element. This model does not change it or assert byte equality between first send and replay.

## Boundaries

No storage schema or durable repository, migration, WebSocket integration, registered endpoint/resource activation, routing cutover, or wire-format change belongs in this slice. The model stores no credentials, permission snapshots, plaintext previd, physical transport generation, or process-local ordinal as durable session identity. It does not prove atomicity, commit durability, writer authority, or XEP conformance for production paths. Future durable custody must fence detached-stream writers before canonical-row deletion and account retirement; this fixture does not establish that fence or prevent late SM-ingress appends.

## Verification

Deterministic tests cover the transition table, competing resumes, displacement, same-socket fallback, late cleanup and compensation, principal mismatch/revocation, pipelined enable/input, unknown commit/write, ACK/exposure/wrap/overflow, append/expose/uncertain-write/retry/replay/ACK, and tombstone retention/custody. The in-crate fixture and its explicit test-utils build do not establish production storage, socket, endpoint, writer-authority, or XEP conformance.
