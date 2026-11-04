# Prepared PostgreSQL first-origin alias race

The prepared PostgreSQL commit path keeps the ingress transaction owned while it
resolves a previously unbound origin alias. It probes the qualified alias,
locks the canonical parent, then rereads the exact alias and digest. On an
initial miss it inserts a candidate without an envelope or source binding inside
a savepoint. A uniqueness loser rolls that savepoint back before probing again;
it does not need to delete a candidate message or update an alias. The normal
commit path fills a newly selected candidate's envelope in the outer transaction.
Admission, stream and room checks retain their existing order and semantics.

A stale probe whose parent disappears or whose alias is absent or rebound
returns `AliasMissingAfterConflict`; a changed or corrupt alias identity returns
`AliasIdentityConflict`. These failures do not mint a replacement and are not
currently automatically retried. This can reject an otherwise valid offer during
concurrent privileged cleanup. Privileged cleanup and alias rewrites must lock
the canonical parent before modifying the alias, or be excluded by ACLs and
quiescence; pool attestation alone does not establish that condition. The
borrowed general repository method is outside the owned-savepoint guarantee. An
ambiguous COMMIT remains ambiguous.

The restricted-role race fixture denies message DELETE and alias UPDATE/DELETE;
that fixture does not establish that relayed, bound-stream, extension or room
paths work under the same narrow grants. The prepared direct/SM acceptance
fixture uses its own broader role contract. Cancellation tests on the new path
cancel after each savepoint command was acknowledged while a following statement
is in flight, and check eventual transaction and lane cleanup. The separate
source-alias laboratory tests command execution with the reply still unobserved;
this change does not establish behavior for a real blackholed network or lost
savepoint reply. Startup remains unchanged and this work does not activate P2.1.

The unprepared SQLite commit path retains its existing unqualified alias lookup,
including its behavior with a connection-local TEMP shadow. The compatibility
check documents that behavior; it is not a SQLite schema-attestation claim.
The resumable/GC-like concurrency test exercises a new wire occurrence that
reuses an existing alias, followed by a parent-lock observer; it does not invoke
the GC sweep or replay an already-bound wire position.
