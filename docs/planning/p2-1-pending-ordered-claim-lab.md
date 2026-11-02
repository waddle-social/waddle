# P2.1 dark ordered-claim observation lab

This test-support-only lab binds one opaque capability to a `DatabasePool`.
It exercises bounded-result ordered claims and same-process positive observation
on the pool's main lane. PostgreSQL requires that pool's physically attested
main lane and its own reserved ingress lane; SQLite requires exactly one pool
connection. It is not wired to startup, the live flush, delivery, ACK, or a
pending-store constructor.

An attempt is minted with a fresh token, recipient, session and maximum of 64
rows before dispatch. A shared one-start latch forbids reusing it, including
after cancellation. A successful claim returns only row identities and their
then-unoffered phase. A later observation finds currently visible matches for
the exact recipient, session, token and row ID; it does not preserve the
original batch or claim order. Empty or mismatched observation is not a proof
of rollback, even after fencing: rows can be released, reclaimed, or deleted.
An offered marker is not proof of enqueue or ACK. The random token is not a
database-unique authorization credential, and the lab assumes trusted writers
and no token collision or injection. Losing the process loses its opaque
attempt; there is no durable pre-dispatch handoff or crash recovery here.

Both claim and observation use capability-derived, fully qualified PostgreSQL
relations or `main` on SQLite. The PostgreSQL transaction fixes the query path
with `pg_catalog` first, reattests the pinned connection, checks it is not in
recovery, and applies server and client wait bounds. That observation is not a
split-brain or old-primary fence. SQLite checks for TEMP triggers on the same
pinned connection and transaction before accessing `main`. The lab fails
closed unless the pending relation has an ordinary-table, nonpartial,
immediate unique row-ID key; PostgreSQL also excludes inherited children and
requires a built-in btree text operator class. SQLite accepts only indexes
using built-in collation names, and assumes the host has not replaced those
collations on the connection.
Those are narrow catalog checks, not an attestation of the pending owner,
complete schema, RLS, main-schema triggers, or MAM shape. Privileged DDL and
owner changes, other untrusted writers, and old backends must be quiescent or
fenced externally throughout this lab.

Returned count and row-ID bytes are bounded; that does not bound the work of
the window query, which can scan and sort the entire recipient backlog. SQLite
has no in-flight deadline here because cancelling the sole private-memory
connection could destroy its database. A PostgreSQL COMMIT timeout, lost reply,
or cancellation remains indeterminate, and no new token or absence-based
retry is authorized by these observations.

The existing live release additionally coordinates a per-session ACK-window
lock and settles ACK windows. This lab deliberately exposes no release because
it cannot share that state from a pool-only constructor. That coordination is
a prerequisite for any production release integration. The claim's MAM ordinal
and the later resolver's newest matching archive/stanza ID are still separate
transactions; a later row with the same stanza ID can change the resolved row.
The lab does not establish stable claim-to-lookup identity or ordering under
concurrent archive writes.
