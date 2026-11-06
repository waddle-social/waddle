# Dark durable SM custody for one local direct frame

Status: draft plan for #2046. No production selector, migration registration,
WebSocket switch, GC activation, or resume activation is included.

## Boundary

Build one shared-store custody path for a prepared local direct frame and one
captured resumable SM session. Commit immutable full-payload canonical admission
and a payload-bound operation journal before accepting a memory queue entry.
Bind the admission to the exact principal, claim, endpoint, SM generation and
checked nonwrapping ordinal. A known committed slot is the only basis for
counting or replay. An unknown commit remains retained until authoritative
reconciliation; absence alone never proves rollback.

## Implementation sequence

1. Define a dark, production-shaped PostgreSQL repository and unregistered
   schema candidate for the operation journal, session gate, full-payload
   admission, immutable slot, exposure and terminal evidence.
2. Admit one prepared direct frame under physical/epoch, principal, claim and
   session fences; preserve the original payload, attempt and receipt time.
3. Bind and count only after known commit; settle by ACK, full-payload transfer
   or authoritative tombstone. Keep the existing live transport path selected.
4. Exercise PostgreSQL 17 concurrent cancellation, unknown commit, late writer
   versus GC, stale claim, wrong payload, ACK/exposure and restart inventory
   with required database execution and no silent skips.
5. Review the global lock order and adverse paths independently before any
   production selector or migration registration.

## Deferred production gates

Every countable live and detached writer, recipient batch, terminal buffer and
replay path must use or be denied by the selected durable counter. Production
selection also requires P2.1 restricted transaction authority, exact P3
session/endpoint lifecycle, canonical-retention and GC fences, uncertain
backend reconciliation, old-writer drain, multiprocess/XEP qualification and
coordinated rollout.
