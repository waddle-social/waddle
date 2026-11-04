# P2.1 durable direct admission

Status: draft implementation plan for #2116 and #2046, stacked on #2157. The
first implementation is dark. It does not activate P2.1, change the live SM
writer, or authorize canonical reclamation.

## First boundary

Cover one already-prepared direct frame to one locally captured, SM-enabled
full-JID connection. Use the exact canonical message, effect intent, resource,
prepared payload, receipt time, send attempt, connection generation, and SM
session identity. Keep the application protocol on XMPP.

In the existing live-send start transaction, lock the canonical parent and
atomically record or recover a stable immutable full-payload admission before
any accepted enqueue. A confirmed commit is required before enqueue. A timeout
or lost commit response requires authoritative lookup by admission and attempt
identity; it must not mint another admission or claim rollback.

At the recipient, durably bind the admission to the exact stream generation and
wrap-aware output ordinal before counting, exposing, or evicting a frame. The
binding and full payload must survive restart independently of the bounded
in-memory replay queue. A confirmed ACK, atomic transfer to pending delivery,
or authoritative tombstone is terminal evidence. Queue acceptance alone is not.
Recovery claims the exact old owner and uses the same identity and payload.

The repository uses the shared correctness store and a private typed transaction
boundary. PostgreSQL needs the installed epoch proof, physical attestation,
schema-qualified relations, bounded waits, and an audited claim/canonical/
admission/SM lock order. SQLite uses the actual shared pool, `BEGIN IMMEDIATE`,
and `main`-qualified relations. No transaction spans socket I/O. Bound both
outstanding admission count and bytes before accepting more work.

## Retention and migration

Canonical GC must retain pending admissions and SM custody under the same
canonical lock. An admission invalidates the old tail; positive settlement of
the last blocker begins a fresh full retention tail. A delayed raw SM writer
must not insert after reclamation. Existing test-only pins are observations,
not a runtime fence or a drop-in live admission implementation.

Activation requires converting or denying every other SM writer and route form,
including room, carbons, recipient-pass, relay, snapshot and drain paths. Old
binary and database sessions must be fenced after a verified drain of sockets,
relay queues, in-flight attempts and already-counted frames. An evicted legacy
frame without durable payload cannot be reconstructed from a key; an unknown
inventory is a hard stop. The owner/epoch/role transition and reconnect identity
must be enforced before enabling production GC or the new writer.

## Verification gates

- Real SQLite and restricted-role PostgreSQL tests for exact admission,
  conflict, retry, unknown commit, wrong connection/session and queue refusal.
- Barriers for GC before/after admission and binding, live queue eviction,
  restart recovery, ACK, promotion, tombstone, detach and resume races.
- Counter wrap, crash after bind before memory count, stale owner and old
  session rejection, bounded pool/admission saturation, and hostile schema
  search path tests.
- Independent review of lock order, recovery custody, old-writer coverage,
  XEP-0198 behavior and activation evidence before any live wiring.

The dark repository and narrow direct-path tests may land before these global
activation gates. They must keep unsupported routes fail-closed if a gated mode
is ever enabled, and must not describe local tests as production proof.
