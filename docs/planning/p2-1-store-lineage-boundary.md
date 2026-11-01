# P2.1 store lineage boundary

This is a dark [#2116](https://github.com/waddle-social/waddle/issues/2116) slice under [#2046](https://github.com/waddle-social/waddle/issues/2046), stacked on the global owner bootstrap PR. It extracts the per-store lineage owner action from server startup and adds a read-only lineage attachment check for an already provisioned store.

## Plan and contract

1. Move the existing `_lineage` ensure/enroll/adopt sequence to `db::bootstrap::bootstrap_store_owner`. Keep the current server wrapper responsible for store logging, matched adoption reporting, and handle registration. Preserve the existing call order and opened-handle ephemeral classification, including MAM and pending delivery.
2. Add `verify_store_lineage_runtime` with the same independently supplied complete expected identity, transaction isolation, trusted schema qualification, and temporary-table shadow defense as global runtime verification. Omit only the global migration ledger requirement. Refactor shared mechanics instead of maintaining two security-sensitive copies.
3. Test file-backed SQLite and isolated PostgreSQL owner/runtime roles, identity mismatches, missing and shadowed lineage, stale pooled transactions, and adoption outcomes. Run formatter, strict Clippy, focused tests, and the repository's required PostgreSQL test inventory.

The store verifier proves the lineage row and physical identity only. It is not wired into startup and cannot prove the store schema, runtime grants, physical attestation function, or release activation. Store constructors currently perform DDL before lineage registration; later work must move those owner writes and establish an independently provisioned target identity before a credential split.
