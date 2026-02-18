# Audit: Half-Baked / Stub / Mock Implementations in XMPP Server

**Status:** ✅ Complete  
**Created:** 2026-02-14

---

## Executive Summary

The XMPP server is **not faked**. The core protocol implementation is real and substantial (~129 `.rs` files). All `AppState` trait methods are wired to real database-backed storage. S2S federation is fully implemented. The compliance suite passes 414/422 tests with 0 failures.

However, there are **real issues** to fix:

| Severity | Count | Summary |
|----------|-------|---------|
| 🔴 Critical | 2 | Dead OAuth state code, unsafe unwraps that will panic |
| 🟡 Moderate | 7 | Hardcoded defaults, permissive CORS, placeholder values, lock-poison panics |
| 🟢 Tech-debt | 5 | Stale docstrings, cosmetic TODOs |
| ⚠️ Unwrap policy | 34 production `unwrap()`, 46 production `expect()` | Must all be replaced with proper error handling |

---

## Final Findings Report

### 🔴 Critical — Must Fix

| # | Location | Category | Details | Recommendation |
|---|----------|----------|---------|----------------|
| 1 | `waddle-server/src/server/routes/xmpp_oauth.rs:183-201` | dead-code | `XmppPendingState` is created with `client_redirect_uri`, `client_state`, `client_code_challenge` — then serialized to JSON and **only logged**. The `_xmpp_state_key` is unused. The callback at line 282+ falls back to reading `params.state` from the query string, meaning the XMPP OAuth state roundtrip is broken for any client that relies on server-side state storage. | Fix: actually store `XmppPendingState` in `pending_auths` (or a dedicated store) keyed by ATProto state, and retrieve it in the callback. |
| 2 | `waddle-xmpp/src/muc/mod.rs:358` | unsafe-unwrap | `self.occupants.get(&nick).unwrap()` — HashMap lookup immediately after insert. Currently safe by construction but a **panic time-bomb** if the insert logic is ever refactored. | Fix: use `.expect("just inserted")` at minimum, or return `Result`. |

### 🟡 Moderate — Should Fix

| # | Location | Category | Details | Recommendation |
|---|----------|----------|---------|----------------|
| 3 | `waddle-xmpp/src/connection.rs:3205-3207` | hardcoded-default | Presence probe response hardcodes `show=None, status=None, priority=0` with `// TODO: Get actual` comments. Connected clients' actual presence state is not returned to probing contacts. | Fix: store per-resource presence state and return it. This affects RFC 6121 compliance. |
| 4 | `waddle-server/src/server/mod.rs:524` | security | `CorsLayer::permissive()` with `// TODO: Configure proper CORS in production`. Allows any origin to make credentialed requests to the API. | Fix: configure explicit allowed origins from env var. |
| 5 | `waddle-server/src/server/routes/websocket.rs:568-569` | placeholder | `"default".to_string()` for both `waddle_id` and `channel_id` when creating MUC rooms via websocket. Rooms won't be associated with the correct waddle/channel. | Fix: derive waddle_id and channel_id from room JID or context. |
| 6 | `waddle-xmpp/src/carbons/mod.rs:150-151,177-178` | unsafe-unwrap | 4× `to_jid.parse().unwrap()` and `from_jid.parse().unwrap()` on dynamic JID strings passed as `&str`. Will panic if a malformed JID reaches carbon forwarding. | Fix: return `Result` from `build_sent_carbon`/`build_received_carbon`, propagate errors. |
| 7 | `waddle-xmpp/src/muc/federation.rs:358,393,427,586` | unsafe-unwrap | 4× `.expect("Nick should be valid resource")` on dynamic nick strings from remote servers. A malicious remote server sending an invalid nick will crash the process. | Fix: return error and reject the stanza instead of panicking. |
| 8 | `waddle-xmpp/src/isr.rs:218-379` | lock-poison | 9× `.expect("ISR token store lock poisoned")` on `RwLock`. If any thread panics while holding the lock, all subsequent ISR operations will panic-cascade. | Fix: handle `PoisonError` gracefully (either reset or propagate error). |
| 9 | `waddle-xmpp/src/roster/mod.rs:562` | unsafe-unwrap | `to_jid.parse().expect("Invalid JID for roster push")` on a dynamic JID string. | Fix: handle parse error and skip the push with a warning. |

### 🟢 Tech-Debt — Nice to Fix

| # | Location | Category | Details | Recommendation |
|---|----------|----------|---------|----------------|
| 10 | `waddle-xmpp/src/connection.rs:4917` | stale-doc | Docstring says "stub implementation that returns an empty roster" but the method delegates to `self.app_state.get_roster()` which is fully wired to `DatabaseRosterStorage`. | Fix: update docstring to reflect actual implementation. |
| 11 | `waddle-xmpp/src/connection.rs:5019` | stale-doc | Docstring says "stub implementation that acknowledges the request but does not persist changes" but the method calls `self.app_state.set_roster_item()` and `self.app_state.remove_roster_item()` which persist to SQLite. | Fix: update docstring. |
| 12 | `waddle-xmpp/src/connection.rs:5095` | cosmetic | Comment "Treat these as idempotent no-op removals" — this is intentional behavior for legacy client compat, not a stub. | No action needed, already correct. |
| 13 | `waddle-cli/src/main.rs:429` | TODO | `// TODO: Store for later / show notification` — incoming message handling in CLI client. | Low priority, CLI is a dev tool. |
| 14 | `waddle-server/src/server/routes/websocket.rs:443-444` | placeholder | `/pending` resource JID created before resource binding. Verified: it IS properly replaced during `handle_resource_binding()`. | No action needed — working as designed. |

---

## Step 3: AppState Trait Method-by-Method Verification

Every `AppState` trait method in `waddle-xmpp/src/lib.rs` has a real production implementation in `waddle-server/src/server/xmpp_state.rs`:

| Method | Production Impl | Storage Backend | Status |
|--------|----------------|-----------------|--------|
| `validate_session` | `SessionManager::validate_session` | SQLite `sessions` table | ✅ Real |
| `validate_session_token` | `SessionManager::validate_session` | SQLite `sessions` table | ✅ Real |
| `check_permission` | `PermissionService::check` | SQLite `relation_tuples` table | ✅ Real |
| `domain` | Returns `&self.domain` | Config | ✅ Real |
| `oauth_discovery_url` | Constructs from `WADDLE_BASE_URL` | Env var | ✅ Real |
| `list_relations` | `PermissionService::list_relations` | SQLite | ✅ Real |
| `list_subjects` | `tuple_store.list_subjects` | SQLite | ✅ Real |
| `lookup_scram_credentials` | `NativeUserStore::get_scram_credentials` | SQLite `native_users` table | ✅ Real |
| `register_native_user` | `NativeUserStore::register` | SQLite with PBKDF2 hashing | ✅ Real |
| `native_user_exists` | `NativeUserStore::user_exists` | SQLite | ✅ Real |
| `get_vcard` / `set_vcard` | `VCardStore::get/set` | SQLite `vcards` table | ✅ Real |
| `create_upload_slot` | Direct SQL insert | SQLite `upload_slots` table | ✅ Real |
| `max_upload_size` | Reads `WADDLE_MAX_UPLOAD_SIZE` | Env var, default 10MB | ✅ Real |
| `upload_enabled` | Returns `true` (default) | Hardcoded | ✅ Real (default trait impl) |
| `get_roster` | `DatabaseRosterStorage::get_roster` | SQLite `roster_items` table | ✅ Real |
| `get_roster_item` | `DatabaseRosterStorage::get_roster_item` | SQLite | ✅ Real |
| `set_roster_item` | `DatabaseRosterStorage::set_roster_item` | SQLite | ✅ Real |
| `remove_roster_item` | `DatabaseRosterStorage::remove_roster_item` | SQLite | ✅ Real |
| `get_roster_version` | `DatabaseRosterStorage::get_roster_version` | SQLite | ✅ Real |
| `update_roster_subscription` | `DatabaseRosterStorage::update_subscription` | SQLite | ✅ Real |
| `get_presence_subscribers` | `DatabaseRosterStorage::get_presence_subscribers` | SQLite | ✅ Real |
| `get_presence_subscriptions` | `DatabaseRosterStorage::get_presence_subscriptions` | SQLite | ✅ Real |
| `get_blocklist` | `DatabaseBlockingStorage::get_blocklist` | SQLite `blocklist` table | ✅ Real |
| `is_blocked` | `DatabaseBlockingStorage::is_blocked` | SQLite | ✅ Real |
| `add_blocks` / `remove_blocks` / `remove_all_blocks` | `DatabaseBlockingStorage::*` | SQLite | ✅ Real |
| `get_private_xml` / `set_private_xml` | Direct SQL queries | SQLite `private_xml_storage` table | ✅ Real |
| `list_user_waddles` | `list_user_waddles_from_db` | SQLite (global DB) | ✅ Real |
| `list_waddle_channels` | `list_channels_for_waddle_from_db` | SQLite (per-waddle DB via pool) | ✅ Real |

**Verdict: All 30 AppState methods are fully implemented with real database storage. Zero stubs.**

---

## Step 4: S2S Federation Assessment

| Module | Lines | Status | Notes |
|--------|-------|--------|-------|
| `s2s/mod.rs` | 191 | ✅ Complete | State enums, metrics, direction types — all real |
| `s2s/dns.rs` | 309 | ✅ Complete | Real DNS SRV resolution via `hickory-resolver`, proper fallback to A/AAAA, priority/weight sorting |
| `s2s/dialback.rs` | 500 | ✅ Complete | Full XEP-0220 implementation with HMAC-SHA256 key generation, result/verify builders |
| `s2s/outbound.rs` | 781 | ✅ Complete | Full connection lifecycle: TCP → STARTTLS → TLS → stream negotiation → dialback auth |
| `s2s/connection.rs` | 526 | ✅ Complete | Inbound connection handling: stream header, STARTTLS, dialback, stanza routing with domain spoofing protection |
| `s2s/pool.rs` | 693 | ✅ Complete | Production connection pool with DashMap, health checks, exponential backoff, idle cleanup, maintenance tasks |
| `s2s/listener.rs` | 187 | ✅ Complete | TCP listener with TLS acceptor, spawns connection actors |
| `muc/federation.rs` | 1299 | ✅ Complete | Full MUC federation: remote occupant join/leave, presence sync, message routing, affiliation management |

**Verdict: S2S federation is fully implemented, not scaffolded. ~3,486 lines of real federation code.**

One concern: the dialback verification in `connection.rs:424-432` does local-only "piggyback" verification rather than the full back-connection verification specified in XEP-0220. This is a simplification noted in code comments, not a stub.

---

## Step 5: HTTP API Route Assessment

| File | Lines | Status | Concerns |
|------|-------|--------|----------|
| `uploads.rs` | 1049 | ✅ Real | Full XEP-0363 file upload/download, slot validation, disk storage, cleanup |
| `waddles.rs` | 2313 | ✅ Real | CRUD operations, membership management, invites. Two `placeholder` comments for user creation with DID handle — these are intentional bootstrap behavior |
| `websocket.rs` | 1042 | 🟡 Partial | XMPP-over-WebSocket works but uses `"default"` waddle_id/channel_id placeholders (finding #5) |
| `device.rs` | 920 | ✅ Real | Device registration, verification codes — `placeholder` hits are CSS `::placeholder` pseudo-elements |
| `xmpp_oauth.rs` | 437 | 🔴 Broken | OAuth state storage is dead code (finding #1). The discovery endpoint and callback redirect work but state roundtrip is broken |
| `server/mod.rs` | ~600 | 🟡 Has TODO | `CorsLayer::permissive()` TODO (finding #4) |

---

## Step 6: Production `unwrap()`/`expect()` Full Inventory

### Counts by crate (production code only, excluding `#[cfg(test)]`)

| Crate | `unwrap()` | `expect()` | Total |
|-------|-----------|-----------|-------|
| waddle-xmpp | 13 | 17 | 30 |
| waddle-server | 6 | 18 | 24 |
| waddle-cli | 3 | 1 | 4 |
| waddle-ecdysis | 0 | 10 | 10 |
| waddle-xmpp-xep-github | 12 | 2 | 14 |
| **Total** | **34** | **48** | **82** |

> **Note:** The earlier estimate of 221 `unwrap()` in waddle-xmpp was inflated by the brace-depth heuristic miscounting `#[cfg(test)]` boundaries. The true production count is **13**.

### Classification Summary

| Risk | Count | Action |
|------|-------|--------|
| SAFE — string literals, constants, guarded checks | 36 | Replace with `.expect("reason")` for consistency, but not urgent |
| RISKY — dynamic data, Mutex/RwLock poison, JID parsing | 38 | **Must fix** — replace with proper `Result` propagation |
| CRITICAL — will panic on bad input | 8 | **Must fix immediately** — carbons JID parsing (4), muc nick parsing (4) |

### All RISKY + CRITICAL instances

| File | Line | Code | Risk | Fix Priority |
|------|------|------|------|-------------|
| `waddle-xmpp/src/carbons/mod.rs` | 150 | `to_jid.parse().unwrap()` | CRITICAL | P0 |
| `waddle-xmpp/src/carbons/mod.rs` | 151 | `from_jid.parse().unwrap()` | CRITICAL | P0 |
| `waddle-xmpp/src/carbons/mod.rs` | 177 | `to_jid.parse().unwrap()` | CRITICAL | P0 |
| `waddle-xmpp/src/carbons/mod.rs` | 178 | `from_jid.parse().unwrap()` | CRITICAL | P0 |
| `waddle-xmpp/src/muc/federation.rs` | 358 | `.expect("Nick should be valid resource")` | CRITICAL | P0 |
| `waddle-xmpp/src/muc/federation.rs` | 393 | `.expect("Nick should be valid resource")` | CRITICAL | P0 |
| `waddle-xmpp/src/muc/federation.rs` | 427 | `.expect("Nick should be valid resource")` | CRITICAL | P0 |
| `waddle-xmpp/src/muc/federation.rs` | 586 | `.expect("Nick should be valid resource")` | CRITICAL | P0 |
| `waddle-xmpp/src/muc/mod.rs` | 358 | `self.occupants.get(&nick).unwrap()` | CRITICAL | P0 |
| `waddle-xmpp/src/connection.rs` | 5634 | `requested_role.unwrap()` | RISKY | P1 |
| `waddle-xmpp/src/muc/owner.rs` | 389 | `room_jid.with_resource_str("unknown").unwrap()` | RISKY | P1 |
| `waddle-xmpp/src/roster/mod.rs` | 562 | `to_jid.parse().expect(...)` | RISKY | P1 |
| `waddle-xmpp/src/isr.rs` | 218-379 | `.expect("ISR token store lock poisoned")` ×9 | RISKY | P1 |
| `waddle-server/src/permissions/check.rs` | 192 | `tuple_subject.relation.as_ref().unwrap()` | RISKY | P1 |
| `waddle-server/src/permissions/check.rs` | 257 | `tuple_subject.relation.as_ref().unwrap()` | RISKY | P1 |
| `waddle-server/src/auth/did.rs` | 47,62 | `.expect("Failed to build HTTP client")` | RISKY | P2 |
| `waddle-server/src/auth/atproto.rs` | 53,74 | `.expect("Failed to build HTTP client")` | RISKY | P2 |
| `waddle-server/src/auth/dpop.rs` | 56,57 | `public_key.x().expect(...)` | RISKY | P2 |
| `waddle-server/src/server/mod.rs` | 285 | `c2s_listener.expect(...)` | RISKY | P2 |
| `waddle-server/src/server/routes/websocket.rs` | 575 | `.expect("Room just created")` | RISKY | P2 |
| `waddle-xmpp-xep-github/src/client.rs` | 213-439 | `self.*.lock().unwrap()` ×11 | RISKY | P2 |
| `waddle-cli/src/app.rs` | 389,408,419 | `.unwrap()` on channel send | RISKY | P2 |
| `waddle-ecdysis/src/restart.rs` | 46,143,147,158 | `.expect(...)` on exe path/CString | RISKY | P3 |

---

## Cross-Reference: Skipped Compliance Tests vs Findings

The 8 skipped compliance tests (from TODO.md) map as follows:

| Skipped Tests | Related Finding? |
|---------------|-----------------|
| 2× roster versioning (condensed pushes, push order) | **No** — roster versioning IS implemented (`get_roster_version`, push logic in `handle_roster_get`). Skips are due to per-item delta tracking not yet implemented. |
| 6× "without initial presence" roster push tests | **No** — these are skipped due to CAAS test harness config, not due to server stubs. |

**None of the skipped tests are hiding stubs.** They are genuine feature gaps or test harness limitations.

---

## Checklist

- [x] Step 1: Full marker catalog (20 markers found, all accounted for)
- [x] Step 2: Deep-inspect known hotspots (6 hotspots verified)
- [x] Step 3: AppState trait method-by-method verification (30/30 real)
- [x] Step 4: S2S federation assessment (complete, ~3,486 lines)
- [x] Step 5: HTTP API route audit (1 broken, 1 partial, 4 real)
- [x] Step 6: unwrap/expect triage (82 total: 36 SAFE, 38 RISKY, 8 CRITICAL)
- [x] Step 7: Final consolidated report (this document)
