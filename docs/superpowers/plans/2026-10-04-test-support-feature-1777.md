# Feature-gated `test-support` state builder for public-API XEP suites (#1777) Implementation Plan

> Post-implementation note: the self dev-dependency in §1 was dropped. It links the crate twice into the lib unit-test binary, and kameo's link-time remote-message registry then panics with duplicate registrations under `--features clustering`. The suite is gated with `#![cfg(feature = "test-support")]` instead and the feature is enabled by the `--all-features` lanes plus `waddle-server-xmpp-server-tests` (the `waddle-xmpp` `test-utils` precedent).

**Goal:** Let `crates/waddle-server/tests/xep0045_*.rs` integration suites build the in-process `WebSocketState` (invite ledger actor, room registry) so XEP-0045 decline-recovery coverage can live in the public `tests/` tree like every other XEP suite, without the release image gaining test code.

**Spec:** GitHub issue #1777 (follow-up to #1755 / PR #1775).

## Design

1. **Cargo feature + self dev-dependency** (`crates/waddle-server/Cargo.toml`):
   ```toml
   [features]
   clustering = [...]
   test-support = []
   [dev-dependencies]
   waddle-server = { path = ".", features = ["test-support"] }
   ```
   Resolver 2 keeps dev-dependency features out of `cargo build --bin` (the image builds with `--locked --package waddle-server --bin waddle-server --features clustering`, `flake.nix:357`; nothing in the release path uses `--all-features`). CI clippy/nextest use `--all-features`, so the gated code is linted with `-D warnings` and compiled into the lib there; that is intended.

2. **Gates widened from `#[cfg(test)]` to `#[cfg(any(test, feature = "test-support"))]`**, narrowly:
   `ServerConfig::test_homeserver` + `test_occupant_id_secret` + `TEST_OCCUPANT_ID_SECRET` (`config.rs:1562-1569, 1659`); `IngressAuthority::for_test` + `test_lineage_config` (`ingress/mod.rs:283, 703`); `AppState::new` (`server/state.rs:104`); `BotAvatars::loopback` (`extension_bot_avatar.rs:225`); `dual_registration::mirror_register` (`dual_registration.rs:110`). `ServerConfig::test_standalone` and `impl Default for ServerConfig` stay `#[cfg(test)]`.

3. **Builder moves out of the `#[cfg(test)]` tests module.** New file `src/server/routes/websocket/test_state.rs`, declared `#[cfg(any(test, feature = "test-support"))] pub(crate) mod test_state;`, holding `TestStateOverrides`, `empty_extension_manager`, `create_test_websocket_state_with_extension_manager`, `seed_local_account`, `create_test_session`, `register_test_connection` (moved verbatim; `tests.rs` re-imports them with `pub(crate) use super::test_state::*` so the ~40 in-crate sibling builders and their callers do not change).

4. **Public surface** `src/test_support.rs`, `#[cfg(any(test, feature = "test-support"))] pub mod test_support;` in `lib.rs`:
   ```rust
   pub use crate::server::routes::websocket::WebSocketState;      // pub item re-exported through the crate-private `routes`
   pub use crate::server::routes::websocket::muc_invites::{
       claim_invite, list_invites, record_invite_at, InviteStorageError, OutstandingInvite, RecordOutcome,
   };
   pub use crate::server::routes::websocket::test_state::{create_test_session, register_test_connection, seed_local_account};
   /// Minimal state over a caller-owned pool and ingress authority: database-backed pending
   /// delivery storage and notification-settings projection on the same database (mirrors the
   /// in-crate `family_state`). The ingress authority must be `IngressAuthority::new` (it owns the
   /// maintenance task) when the test drives recovery through `trigger_maintenance`.
   pub async fn websocket_state_with_ingress(db_pool: Arc<DatabasePool>, ingress: Arc<IngressAuthority>) -> Arc<WebSocketState>;
   ```
   `muc_invites::{record_invite_at, list_invites, claim_invite}` become `pub` (the module stays `pub(crate)`; the re-export is the only public path). Nothing else widens. No `unwrap`; the helpers keep `expect` because they are test-only by construction (documented in the module doc and the PR).

5. **Suite move.** `src/ingress/xep0045_decline_recovery_tests.rs` is deleted (and its `#[path]` mod line in `recovery_executor_tests.rs:2504-2505`), and the four cases × {sqlite, postgres} are re-created in `tests/xep0045_invitation_decline_recovery.rs` through the public API:
   - `mod ingress_support; use ingress_support::IngressFixture;` (existing public fixture);
   - `let authority = Arc::new(fixture.authority().await); let state = waddle_server::test_support::websocket_state_with_ingress(pool, authority.clone()).await;`
   - `plan_message_dispatch(&mut machine, message, &state.recovery_deps())` via `waddle_server::ingress::RecoveryEnvironment` (replaces `build_interpret_deps`);
   - `commit_submission(&fixture.uow, &submission, 5)` (public);
   - partial receipts via the public `EffectReceiptKind::from_storage(intent.with_encoded_v1(|k, _| k))` + `Sha256(intent.semantic_key().storage_identity())` pattern (`tests/ingress_cases/pending_reconstruction.rs:308-327`), replacing `receipt_key`;
   - recovery driven the public way: `authority.bind_recovery_environment(Arc::downgrade(&env))`, backdate `ingress_messages.created_at` by 120 s **before** reading `canonical_created_at` (so the reinvite ordering `invitation_created_at < replacement_created_at = canonical - 1 min` still holds), `authority.trigger_maintenance()`, poll `fixture.count("ingress_messages WHERE terminal_at IS NOT NULL")` under a 15 s timeout; second pass = commit a sentinel submission, trigger, wait for n+1, then re-assert nothing was resent;
   - `family_recovered` reproduced with `fixture.uow.begin()` + `EffectReceiptRepository::receipts_complete` + `CanonicalMessageRepository::is_terminal` + `fixture.count("ingress_effect_receipts")`;
   - end with `authority.drain_and_join(15 s)`, drop state, `fixture.close()`.
   Assertions are carried over one-for-one (decline forwarded with `from == room`, `decline/@from == invitee`, reason text; no resend on partial route/fallback receipts; newer invitation preserved with claim counts; `pending_delivery == 0`).

## Tasks (TDD seams: the public `waddle_server::test_support` API and the moved XEP-0045 suite)

1. Feature + self dev-dep; `cargo check -p waddle-server --features test-support` and default both compile. Red: a `tests/xep0045_invitation_decline_recovery.rs` stub importing `waddle_server::test_support::websocket_state_with_ingress` fails to compile.
2. Widen the five gates; move the builder into `test_state.rs`; add `test_support.rs` with the public builder and re-exports. Green: stub compiles; in-crate suites unchanged and green.
3. Port the suite (one case at a time: plain decline → route receipt → fallback receipt → reinvited), delete the in-crate file and its mod line. Each case red-then-green on SQLite, then run its Postgres half.
4. Docs: `server/AGENTS.md` XMPP conformance testing section gains one line: "In-process server state for `tests/xepNNNN_*.rs` comes from `waddle_server::test_support` (feature `test-support`, enabled for the crate's own tests via the self dev-dependency; never enabled in the release image)."

## Verification

`cargo fmt`; `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo clippy -p waddle-server --all-targets -- -D warnings` (default features; the self dev-dep enables `test-support` for test targets); `cargo build --locked -p waddle-server --bin waddle-server --features clustering` (release shape) and confirm with `cargo tree -p waddle-server -e features --features clustering | grep test-support` that the feature is absent; `cargo nextest run -p waddle-server --features test-support --test xep0045_invitation_decline_recovery` with `WADDLE_TEST_POSTGRES_URL` set (the suite is feature-gated, so without the feature zero tests run); `cargo nextest run -p waddle-server --lib ingress::` to prove the in-crate recovery suites still pass; full workspace nextest once at the end.
