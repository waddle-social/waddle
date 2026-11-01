# Local SQLx PostgreSQL patch

This directory is the published `sqlx-postgres` 0.8.6 crate from crates.io.
Its license files and otherwise unchanged source are retained. The workspace
patch in `server/Cargo.toml` selects it in place of the registry copy.
The local `.gitattributes` exempts upstream trailing spaces from Git's
whitespace check; the crate contents otherwise match the published archive
except for `src/transaction.rs`.

## Interrupted `BEGIN` rollback

`src/transaction.rs` moves the transaction-depth increment ahead of the
`wait_until_ready()` await and undoes it when `BEGIN` did not take. This is a
backport of [SQLx PR #4394](https://github.com/transact-rs/sqlx/pull/4394)
(`7ff8045`, merged 2026-09-09). A cancelled `BEGIN` can otherwise return a
PostgreSQL connection to SQLx's pool while the server still has an open
transaction; [SQLx issue #4423](https://github.com/transact-rs/sqlx/issues/4423)
contains a deterministic PostgreSQL reproduction and a separate confirmation
on 0.8.6. The Waddle regression is
`tests/sqlx_cancelled_begin.rs::cancelled_ordinary_begin_never_reuses_an_open_postgres_transaction`.

Keep this patch until a released SQLx version with the same fix is adopted and
the Waddle regression passes against it. This patch does not claim to resolve
the separate failed multi-statement transaction case discussed in SQLx PR
[#4409](https://github.com/transact-rs/sqlx/pull/4409).
