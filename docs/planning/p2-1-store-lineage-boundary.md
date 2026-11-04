# P2.1 store lineage boundary

This is a dark [#2116](https://github.com/waddle-social/waddle/issues/2116) slice under [#2046](https://github.com/waddle-social/waddle/issues/2046), stacked on the global owner bootstrap PR. It extracts the per-store lineage owner action from server startup and adds a read-only lineage attachment check for an already provisioned store.

## Plan and contract

1. Move the existing `_lineage` ensure/enroll/adopt sequence to `db::bootstrap::bootstrap_store_owner`. Keep the current server wrapper responsible for store logging, matched adoption reporting, and handle registration. Preserve the existing call order and opened-handle ephemeral classification, including MAM and pending delivery.
2. Add `verify_store_lineage_runtime` with the same independently supplied complete expected identity, transaction isolation, trusted schema qualification, and temporary-table shadow defense as global runtime verification. Omit only the global migration ledger requirement. Refactor shared mechanics instead of maintaining two security-sensitive copies.
3. Test file-backed SQLite and isolated PostgreSQL owner/runtime roles, identity mismatches, missing and shadowed lineage, stale pooled transactions, and adoption outcomes. Run formatter, strict Clippy, focused tests, and the repository's required PostgreSQL test inventory.

The store verifier proves the lineage row and physical identity only. It is not wired into startup and cannot prove the store schema, runtime grants, physical attestation function, or release activation. Store constructors currently perform DDL before lineage registration; later work must move those owner writes and establish an independently provisioned target identity before a credential split.

The main-lane `DatabaseLineageAttestor` now uses that verifier for a physically
attested PostgreSQL pool, including a read-only transaction and the bounded
pre-snapshot lock/statement settings. A previous successful probe is not reused
when such an attested checkout or probe is unavailable: SQLx can turn an
identity refusal into the same timeout as pool saturation. Raw PostgreSQL and
SQLite keep their existing probe and sticky-unavailability behavior. This does
not independently establish expected-identity provenance, audit runtime grants,
fence owner DDL, or attach a control-plane pool. The unavailable result also
does not latch or fence an already-serving node under the current health policy;
active-session and node-authority fencing remain production gates.

Physical checkout hooks temporarily cap lock and statement waits at 5 and 10
seconds per statement before reading identity, without loosening tighter session
limits; the previous settings are restored before the connection is lent. A
cancelled readiness probe can report failure before PostgreSQL ends its blocked
hook, leaving a backend occupied until the applicable server-side timeout. These
dark defaults are provisional, and do not bound an unrelated operation already
running on a borrowed connection before the hook begins.
