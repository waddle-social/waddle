# P2.1 commit uncertainty barrier (proposal)

Status: architecture gate blocked. This document records the proof still needed
for #2116 and #2046; it does not authorize retry, source-frontier advancement,
or a P2.2 mailbox cutover.

## Why the current pilot is insufficient

A cancelled PostgreSQL commit can leave its backend working after the caller
returns. An ordinary lookup that sees no canonical row does not prove rollback.
The terminal-custody spike retains a process-local identity guard only after a
claim has minted one. It cannot protect a submission with no claim row, survive
process death, or establish the transaction outcome from a fresh Sync/idle
response. The original connection must not return to a pool while its protocol
terminal state remains unproven; a local guard leak is not a durable fence.

The originless relayed rejection path has a separate identity gap. In
`ingress/commit/implementation.rs`, the planned-rejection branch precedes the
relayed-parent branch. A relayed denial with no origin ID therefore mints a new
`MessageKey`. Non-rejection relayed lookup checks the supplied parent's digest,
but digest v1 does not bind the original sender or source authority. Neither
path is adequate proof for a remote-owner result.

## Candidate operation barrier

Every first attempt, retry, recovery worker, and successor would take the same
transaction-scoped PostgreSQL lock *set* for its pre-known logical identities.
A resumable source occurrence needs its exact SM fence even when an origin ID
is present; the origin alias needs its own lock because the same alias may
appear at another position. The locks must compose with existing admission,
room-claim, stream, canonical-row, and owner-result locks in one reviewed order.
The current commit path takes admission/room proof before the SM lock and
resolves the alias afterward; the proposed order and every recovery/GC path
still need a deadlock audit before implementation. After acquiring the full
set, a separate fresh `READ COMMITTED` statement would check both the source
occurrence and alias bindings, then the canonical outcome and immutable
request evidence, before any write. The locks stay held through commit or
rollback. A delayed original performs the same checks after a successor wins.
Only an exact matching committed occurrence is reused; disagreement fails
closed. The semantic digest is checked evidence, not a lock identity.

These locks serialize matching occurrences and aliases. They do not replace
the exact claim-row fence for current owner authority. A no-claim path needs its own
durable authority proof or a narrower stated guarantee. The design also
depends on a single fenced primary and a failover policy that retains
committed canonical evidence.

| Submission | Candidate lock identity | Required binding |
| --- | --- | --- |
| Origin ID | Typed sender bare JID, normalized target kind/value, origin ID; also the exact source-occurrence fence when one exists | Alias to the first canonical message, payload digest, source principal and original lane/position |
| Resumable occurrence, with or without origin ID | Stable `SmIngressId` plus a nonwrapping ingress ordinal, or `SmIngressId`, `wire_generation`, and `wire_h` if that generation is durable before the first attempt | Canonical message or distinct rejection result, source principal, payload, and bounded-window/overflow proof |
| Originless ephemeral | No recoverable identity exists today | A new key and its original source binding must be durably discoverable by a successor *before* the uncertain transaction; otherwise decline cross-process automatic recovery |
| Remote-room owner acceptance | Canonical `MessageKey` and acceptance variant, after source binding is checked | Original sender, target, source authority, payload and one closed accepted/rejected result |
| Host-owned `Extension` / `RoomResult` | Outside this pilot until their grant/publication identities are specified | Typed host authority and an occurrence binding recoverable before an uncertain commit |

Sibling resources and owner changes must resolve to the first canonical
responsibility rather than minting a new operation. A later identical MUC join
is a new occurrence. A changed payload under an existing origin alias is a
conflict even when it has the same alias-lock identity. The alias remains bound
to the first message: a conflicting rejection has a separate `MessageKey` and
needs its own durable source-occurrence binding to recover after an uncertain
commit. Two distinct conflicting occurrences must not be collapsed merely
because they have the same alias and payload. For ephemeral work, handing a
successor a saved key in a test is not proof that it can rediscover the key
after the origin process dies.

## Falsifiable PostgreSQL pilot

Use a disposable database, independent connections, and the intended restricted
runtime role. Block the first transaction inside COMMIT using a controlled
deferred trigger, then cancel its client while independently observing that the
backend remains active. An ordinary lookup may see no row; a successor taking
the same lock set must wait rather than decide absence. Release the
trigger and require the successor to recover the exact committed result.
Repeat with forced rollback and with the successor winning first; only one
matching outcome may commit.

Repeat the experiment with no claim row, cancellation during BEGIN, a pooled
connection checked out after cancellation, conflicting payload under one
alias, same SM occurrence with alias A/B/absent, the same alias at two SM
positions, and an alias-conflict rejection whose COMMIT reply is lost. Retry
that conflict and submit two distinct same-content conflicting occurrences;
the original accepted message must remain distinct and each proven rejection
may advance its source frontier only once. Include cancellation and
successor-first schedules for each cross-key race. Exercise originless SM
replay before and after the 32-bit `h` wrap, including an uncertain commit at
the wrap; relayed source mismatch and denial; and a successor process after
owner loss. Kill the origin process and require any ephemeral occurrence key
to be rediscovered from surviving authority, not a test-only saved variable.
Assert that an unproven connection is never
lent with an open transaction. Test the actual role's schema, lock-function,
read, write and claim privileges, including revoked/invalid authority. A
compliant-code lock does not by itself prevent raw runtime DML from bypassing
the protocol; the role boundary must be explicit.

Until that pilot and the source-binding contract pass, ambiguous commit stays
non-advancing and cannot trigger automatic retry or effect execution.

## Dark source-binding implementation boundary

V1028 adds nullable source-principal and optional SM-occurrence columns, an
owner-decision latch, and a one-child rejection relation. Repository primitives
can stamp and validate the tuple, check a source against its committed
`DispatchToRoomRemote` intent, and retain a rejection child through its own
retention. Legacy rows stay unbound; an ordinary local origin replay may still
resolve its old alias without changing that row, but the row cannot authorize a
later relayed owner result.

The eligible lane is an authenticated message sent toward a clustered room.
The proof uses the canonical source key, exact room, and committed intent
variant, with the existing current room-claim fence. The recorded relay target
is a routing hint and may change after room ownership transfers. Ephemeral
sources have no SM occurrence; resumable sources carry their typed stream ID
and ordinal. That pair is the immutable first-origin coordinate. A later
authenticated same-origin retransmission may carry a different valid SM
occurrence; the existing SM-reference path binds that occurrence to the same
canonical key without replacing the first coordinate. A relayed owner result
does not carry an SM occurrence, so this stored coordinate cannot prove the
relay's occurrence by itself. Extension and room-result host submissions do
not have this authenticated source tuple; they remain outside this proof
contract.

These schema and repository pieces are dark: the live `commit_attempt` path is
not wired to them, and V1028 adds no runtime-role grants. A separate owner-side
grant lab and a test-only closed owner-result UOW now cover the exact relation
and child error composition, but neither is wired into startup or relay work.
Full origin stamping, alias and claim proof, planner provenance, and the
independent PostgreSQL commit-unknown pilot remain separate work. Likewise,
`record_message_with_source` preserves local replay of a legacy-null alias by
returning successfully without backfilling it; that is not proof of a stamped
source. Activation must return a typed bound/unbound result or assert source
binding in the same UOW before publishing relay work.

The collector patch keeps a rejected source and mapped child through both
retention tails, then locks and deletes them together in one transaction.
SQLite replay, custody and rollback cases pass locally, including candidate
starvation and pending archive custody. The restricted-role grant and paired
collection still need real PostgreSQL execution and commit-unknown testing.
None of these dark labs closes the originless relayed rejection gap or
establishes P2.1 safety.
