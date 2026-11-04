# P2.1 SM owner bootstrap

This is a dark [#2116](https://github.com/waddle-social/waddle/issues/2116) slice under [#2046](https://github.com/waddle-social/waddle/issues/2046), stacked on the per-store lineage boundary. The independent architecture gate cleared an SM-only extraction; pending delivery is a separate follow-up because its initializer includes grants, V1014 claim reset, batched MAM cleanup, and recovery-order work.

## Plan

1. Name the existing portable SM schema initializer as an explicit owner operation. Keep its SQL, per-statement boundaries, legacy column repair, driver behavior, and public test/open behavior unchanged.
2. Name the distinct clustered PostgreSQL fenced initializer as its own owner operation. Preserve validation of PostgreSQL and live claim handles before any schema write, and keep HTTP colocation validation before the SM factory. The current pending-before-SM startup order and SM alias of global stay intact.
3. Test fresh and repeated portable SQLite and PostgreSQL bootstrap, fenced PostgreSQL bootstrap, and restricted-role refusal of owner schema writes. Run focused SM tests, Rust formatting, strict Clippy, and PostgreSQL test inventory.

The owner bootstrap slice did not include a runtime credential switch or read-only SM schema attachment. The existing constructors still call owner bootstrap during startup with the existing credential; other store constructors and pending delivery still write before runtime attachment can be enabled.

## Portable PostgreSQL runtime attachment

`DatabaseSmPersistence::attach_existing_postgres` is a dark, explicit
prerequisite. It accepts only the supplied attested PostgreSQL main lane and
inspects the owner-prepared portable SM tables in one bounded read-only
repeatable-read transaction. It locks the qualified tables before establishing
its snapshot, then checks the active attested schema and OID, required table and
column shapes, primary keys, the ingress disposition check,
and the portable initializer's indexes, including the unique dedup predicate.
The profile comes from the portable owner initializer and the V1012, V1019,
and V1027 PostgreSQL migrations; a retained `shadow_ordinal` requires owner
preparation. Attachment neither repairs schema nor invokes bootstrap.

This is a structural snapshot, not a durable guarantee against later owner DDL,
a check of row contents or runtime DML grants, or proof of operational writer
readiness. It does not attach fenced SM, apply a migration or version policy,
change normal startup, or activate production. Runtime role privileges,
concurrent schema changes, and ownership fences need separate controls.
