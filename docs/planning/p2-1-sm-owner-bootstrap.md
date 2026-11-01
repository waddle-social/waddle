# P2.1 SM owner bootstrap

This is a dark [#2116](https://github.com/waddle-social/waddle/issues/2116) slice under [#2046](https://github.com/waddle-social/waddle/issues/2046), stacked on the per-store lineage boundary. The independent architecture gate cleared an SM-only extraction; pending delivery is a separate follow-up because its initializer includes grants, V1014 claim reset, batched MAM cleanup, and recovery-order work.

## Plan

1. Name the existing portable SM schema initializer as an explicit owner operation. Keep its SQL, per-statement boundaries, legacy column repair, driver behavior, and public test/open behavior unchanged.
2. Name the distinct clustered PostgreSQL fenced initializer as its own owner operation. Preserve validation of PostgreSQL and live claim handles before any schema write, and keep HTTP colocation validation before the SM factory. The current pending-before-SM startup order and SM alias of global stay intact.
3. Test fresh and repeated portable SQLite and PostgreSQL bootstrap, fenced PostgreSQL bootstrap, and restricted-role refusal of owner schema writes. Run focused SM tests, Rust formatting, strict Clippy, and PostgreSQL test inventory.

No runtime credential switch or read-only SM schema attachment is included. The current constructors still call owner bootstrap during startup with the existing credential; other store constructors and pending delivery still write before runtime attachment can be enabled.
