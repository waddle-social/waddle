# P2.1 pending-delivery shared-pool resolver

This dark, test-support-only adapter resolves pending-delivery archive pointers
through the actual `ArchiveResolver` interface. Its closed resolver uses the
same `DatabasePool` main lane as its test-only pending store, and PostgreSQL
requires the exact physically attested pool and its reserved ingress lane. The
MAM relation is selected from the attested schema and the `StanzaId`'s bare
assigning JID. The connection helper uses an archive ID or stanza ID, chooses
the newest archive ordinal, and never interprets an origin ID as a lookup key.

The PostgreSQL resolver bounds the entire lookup, including checkout and
rollback, caps server-side waits in its read-only transaction, and reattests
that transaction's physical connection. SQLite uses the supplied pool and an
ordinary transaction containing only the qualified `main.mam_messages` read;
an in-memory pool must have exactly one connection. SQLite has no added
in-flight operation timeout, to avoid closing its only private-memory connection.
The read transaction must
roll back successfully before a MAM decode result or miss is classified and
before XML parsing or a socket or actor operation. A read, attestation, or
rollback failure is transient; a genuine miss or the existing permanent MAM
decode/XML errors remain poison-pill misses. The ordered flush releases its
claimed suffix on transient failure and retains its existing owner/dispatch
fences. In the responsive-backend test, deadline cancellation eventually
returns the main-lane permit and releases the claims. A stalled backend may
instead keep claims retained for existing retry or janitor handling; this test
does not model a blackholed socket or establish backend termination. SQLite
cancellation coverage is limited to waiting for its single checkout, and does
not establish survival after an in-flight connection is closed. These tests do
not establish cross-process commit recovery.

The test-only pending constructor skips initialization and does not verify the
pending schema, grants, or writer role. The owner must initialize the fixture
first. The existing MAM attachment checks the wider combined ingress/MAM role,
including its write privileges; this is not a proof of a SELECT-only role.
Ordered pending claims also read an unqualified MAM relation before resolution.
The PostgreSQL laboratory relies on the attested checkout's normalized search
path and TEMP restrictions for that claim; the claim SQL itself is not
qualified. SQLite coverage is resolver-only: its ordered claim can see a TEMP
shadow while the resolver reads `main.mam_messages`. The claim and lookup also
use separate transactions; a newer row with the same stanza ID can appear
between them, so this slice does not prove concurrent archive ordering.
Runtime must separately address that query, owner-only pending initialization,
all other MAM operations, old sessions, privileged DDL/ACL quiescence, and
primary fencing before replacing current startup or claiming pool unification.
The adapter does not change runtime startup, roles, epoch, or migrations.
