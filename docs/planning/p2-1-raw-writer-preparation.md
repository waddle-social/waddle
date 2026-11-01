# P2.1 raw ingress writer preparation

Status: implementation plan for the dark, epoch-zero preparation stacked on #2149.

## Boundary

Prepare first-party raw PostgreSQL transactions which can write any guarded ingress table to use READ COMMITTED from the start and obtain the live epoch before application or row locks. Cover effect descendant ancestry, notification outbox drain, publication, legacy adoption and pruning, and push publication, settlement and retirement callers. Preserve SQLite and stores without the ingress foundation. Reject epochs this binary does not support even if a caller supplies valid transaction-local guard settings.

This work does not register V1028, change V1009 or its checksum, activate an epoch or custody, provision roles, alter startup pools, or change protocol wire behavior. It does not assert physical attestation or claim the later relation-lock migration has been installed.

## Approach

1. Add a shared preparation path invoked immediately after the actual transaction begins. Set PostgreSQL isolation before any timeout or catalog SELECT, bound the initial epoch wait, detect an absent foundation for standalone stores, then acquire and validate the supported live epoch before other locks. Preserve tighter existing bounds and SQLite's transaction behavior.
2. Route all audited raw transaction entries that can reach guarded descendant writes through the preparation path, including registration and node retirement. Review caller-owned raw transaction APIs and avoid claiming a proof of lock ordering when the caller can acquire earlier locks.
3. Make the substrate's own transaction start use READ COMMITTED and acquire the epoch before returning; reject unsupported live epochs in its raw write operations. Keep existing epoch guard semantics unchanged.
4. Add deterministic PostgreSQL regressions with a repeatable-read connection default and identified epoch waiters, including legacy adoption and representative descendant/outbox/push paths. Test unsupported future epochs with otherwise valid GUCs, plus SQLite and absent-foundation controls. Add required PostgreSQL tests to the gating inventory.

## Verification and remaining gates

Run `cargo fmt`, strict Clippy (`-D warnings`), required PostgreSQL inventory checks, and focused tests against real PostgreSQL. An independent final diff review must check entry-point coverage, lock ordering, error handling, and preservation of standalone behavior. Installation lock ordering, schema qualification and catalog binding for any future relation-lock migration, role ownership, old binary drain, and activation remain separate work and require their own evidence.
