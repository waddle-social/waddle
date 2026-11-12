//! Account-resource authority: the durable binding rows and the account
//! coordinator over single-node SQLite and, with `test-support`, the
//! in-memory store.
//!
//! PostgreSQL attachment, the clustered claim fence, the deferred session
//! reference under PostgreSQL and commit-uncertainty schedules need a
//! database fixture and are not covered here.

mod coordinator;
mod store;
mod support;
