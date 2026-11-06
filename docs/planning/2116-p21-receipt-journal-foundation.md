# Receipt-operation journal foundation

Status: **unselected and not a serving authorization**. V1030 adds an empty
receipt-operation result table on SQLite and PostgreSQL. On PostgreSQL it also
creates a fixed receipt-and-journal function as `SECURITY INVOKER`, with all
non-owner EXECUTE grants revoked. The migration strips all non-owner table and
function grants inherited through the migration owner's default privileges
before the objects become visible. It does not install the candidate ingress
API, change the epoch, alter legacy write behavior, grant runtime access, or
perform the later role transition.

The migration ledger is append-only. A binary whose migration catalog ends
before V1030 refuses to restart after V1030 is applied with `UnknownVersion`;
rollout and rollback must account for that refusal and drain older binaries and
backends before a later role transition. An old runtime that also owns the
schema may still write the new table after V1030 commits. A future trusted
promotion must therefore fence and drain those writers, take lineage then
ledger then epoch `ACCESS EXCLUSIVE` in the reviewed owner protocol, and verify
an empty journal, its exact owner/object identities and ACLs on that same
transaction before transferring the function to the API owner. No such
promotion is implemented here. Merely checking grants after promotion cannot
prove that the journal was never preseeded.

The separately callable `ReceiptOperationAuthority` owns a lexical transaction
on the exact supplied attested main pool. For PostgreSQL its experimental
strict-v2-plus-journal profile requires independent schema, schema-owner,
runtime, API-owner, epoch, journal-relation and function OIDs. It checks the
pinned heap epoch before locking it, takes shared lineage and ledger advisory
locks followed by epoch `ACCESS SHARE`, and refreshes physical and catalog
identity on the same READ COMMITTED transaction after those advisory and epoch
waits, before the receipt operation. The journal
profile verifies the fixed function body, OID, API ownership and ACL, and the
journal's structure, OID, owner and ACL before the receipt function or journal
table is accessed.
Strict v2 remains a separate diagnostic and rejects the extra API-owned
function once it is provisioned. The transaction retains its locks until
commit, rollback, or drop. The separately installed physical attestor and
provider/DBA role and catalog mutation policy remain trusted prerequisites:
epoch `ACCESS SHARE` does not fence a noncooperating mutator.

This experiment also requires a trusted-clean catalog at entry. The inherited
v2 verifier reads `ingress_epoch_guard_manifest` before the later structural
manifest proves that relation and its column types. A pre-existing hostile
owner could arrange executable cast or operator behavior during that read.
Selection requires catalog-only type and object preflight before any such
application read, a post-wait recheck, and a negative PostgreSQL witness; the
new journal checks do not repair that earlier v2 ordering.

The owner-controlled provisioning expected by this dark profile gives the
NOLOGIN API owner SELECT and INSERT on the journal and ownership of the fixed
SECURITY DEFINER function; the runtime cannot insert, update or delete journal
rows and receives only EXECUTE on that function. **The present experiment also
grants runtime SELECT on the whole journal**, exposing other operation,
message, kind and hash metadata. This is a blocker for serving: replace the
table SELECT with a separately verified exact-key definer lookup and extend the
joint profile and independently pinned role/function matrix before selection.
The runtime credential and application code are trusted. A caller with that
credential can invoke the fixed function directly with caller-supplied fields;
the journal proves an atomic database receipt transaction, not which Rust
caller invoked it or whether an external effect succeeded.

Each caller retains a stable operation UUID and the full receipt tuple before
awaiting. The fixed operation records the receipt and immutable original
`inserted` result together. An existing ID with the same tuple returns its
original result; a conflicting tuple raises and rolls back. An acknowledged
commit is reported as committed; a timeout or failed commit is indeterminate.
After a dropped future, the caller must retain its own UUID. Fresh exact
positive journal readback proves the historical receipt transaction committed;
it does not prove the receipt still exists after collection, the source effect
succeeded, or that any new effect may run. Missing or failed readback does not
prove rollback and does not permit an automatic retry.

There is no journal quota, expiry, tombstone, or deletion protocol. A runtime
caller can create unbounded fresh operation IDs for a valid receipt. A bounded
admission and retention policy that preserves ambiguous-commit reconciliation
is required before serving. The journal has no foreign key to canonical rows,
so historical evidence deliberately survives their collection; this is not a
complete retention policy.

SQLite uses the caller's existing `Database` handle and `BEGIN IMMEDIATE`, with
receipt and result in one transaction. It has no PostgreSQL role boundary: any
other holder of a database guard can alter or forge the journal. Its results
are cooperative same-process evidence, not independently attested authority.
Closed ownership of the actual shared backend remains a serving requirement.
The joint store role profile, universal source and SM identities, global
application lock order, old-writer drain, all entrant conversions, quota and
retention, and unknown-commit recovery beyond positive receipt evidence remain
separate cutover gates. No restricted serving selector is enabled by this
foundation.
