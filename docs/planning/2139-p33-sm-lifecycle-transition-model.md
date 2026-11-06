# P3.3 dark SM lifecycle transition model

Status: implemented as an inactive fixture for #2139 under #2046. This is a deterministic, in-memory test/test-utils model; it does not add production behavior.

## Model

The lifecycle states are EnablingLive, Live, DetachedResumable, TransferHydrating, and Terminal. Identity checks use the exact BindingRef and ConnectionRef values. A distinct checked, nonwrapping session-generation value fences durable-session identity; physical transport generations and process-local ordinals are not session generations.

Enabling records an exact-fenced commit start before a modeled durable-commit receipt makes the enabled effect available. Terminalization retains repair custody if that commit is still in flight; a late unknown result and an exact authoritative resolution can settle it without reviving the session or releasing buffered input. Pre-commit pipelined input is explicitly capped at 64 entries in this fixture; excess input is rejected without mutation. Resume is planned as one atomic compare-and-set over the expected authenticated principal, session generation, claim, binding, ACK, and endpoint/resource installation state. Only the exact publication and hydration receipt completes TransferHydrating and permits replay or ingress. A losing, stale, mismatched, or revoked owner does not mutate state.

Terminal evidence is retained as tombstone/repair custody. Late cleanup can only apply to its exact binding and generation. Unknown commit and uncertain frame-write outcomes retain repair responsibility.

## Frontier

The frontier maintains nonwrapping append, exposure, peer-ACK, and contiguous-ingress ordinals with peer_acked <= exposed <= appended. New exposure is recorded immediately before its corresponding attempt, and every live frontier mutation (append, attempt, write-result, ACK) requires the exact current lifecycle fence. One frame attempt can be in flight per transport; its result must be resolved before the next ordinal can be attempted, and the same transport cannot retry an ordinal. An unknown frame-write result seals further attempts on that transport. An authoritative ACK can resolve the prefix; otherwise only a completed, exactly receipted resume establishes a new transport cursor at peer_acked + 1. The new transport must replay retained rows in order before later ordinals. Replay includes rows never exposed. Wire h values map only within the 32-bit half-space and the model rejects out-of-ceiling ACKs before mutation.

Existing replay behavior adds a delay element. This model does not change it or assert byte equality between first send and replay.

## Boundaries

No storage schema or durable repository, migration, WebSocket integration, registered endpoint/resource activation, routing cutover, or wire-format change belongs in this slice. The model stores no credentials, permission snapshots, plaintext previd, physical transport generation, or process-local ordinal as durable session identity. It does not prove atomicity, commit durability, writer authority, or XEP conformance for production paths. Future durable custody must admit the full payload before queue acceptance against the exact canonical obligation, resource, attempt, endpoint, and session; commit the writer epoch under the canonical lock before admission succeeds; block GC while admission is pending; and atomically fence the SM generation/ordinal/payload/claim update. Terminal ACK, transfer, and tombstone transitions must consume that custody. This fixture does not establish those fences or prevent late SM-ingress appends after canonical-row deletion.

## Verification

Deterministic tests cover the transition table, competing resumes, displacement, same-socket fallback, late cleanup and compensation, principal mismatch/revocation, pipelined enable/input, unknown commit/write, ACK/exposure/wrap/overflow, append/expose/uncertain-write/retry/replay/ACK, and tombstone retention/custody. The in-crate fixture and its explicit test-utils build do not establish production storage, socket, endpoint, writer-authority, or XEP conformance.
