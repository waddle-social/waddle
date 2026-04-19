# TODO.md

## Test runtime snapshot (2026-04-18)

- `cargo test -p waddle-xmpp` (full suite): **~12s** (~1,650 tests)
- `cargo test -p waddle-xmpp --lib` (unit only): **~1.4s** (~1,586 tests)
- `cargo test -p waddle-xmpp --test protocol_conformance`: **~5s** (20 tests)

All tests are native Rust — no Docker, CAAS, or external dependencies required.

## Recently landed — MIX + social scaffold (2026-04-18)

Parallel scaffold of MIX (XEP-0369 + 0405 + 0407) and missing social
features. All modules ship with dedicated integration suites per the XEP
test-suite rule in `CLAUDE.md`.

- MIX core (`src/mix/`, `src/xep/xep0369.rs`) — typed `MixChannel`,
  `MixChannelRegistry`, join / leave / setnick / update-subscription
  round-trip parsers, federation hook.
- MIX-PAM (`src/xep/xep0405.rs`, `src/mix/pam.rs`) — `MixRoster` and
  `client-join` / `client-leave` IQ parsers for the user's own server.
- MIX misc (`src/xep/xep0407.rs`) — invite request + invitation carry,
  disco feature bundle `mix_disco_features()`.
- Inbox (`src/inbox/`, `src/xep/xep0430.rs`) — in-memory `InboxView` +
  `InboxStorage` trait + `InMemoryInboxStorage`, protocol query /
  mark-read IQ, typed `ConversationKind { Direct, MixChannel }`.
- PEP Mood / Activity / Tune (`src/xep/xep0107.rs`, `0108.rs`,
  `0118.rs`) — fully typed payloads, retraction form, disco feature
  constants.
- Encrypted SFS (`src/xep/xep0448.rs`) — closed `Cipher` enum
  (AES-128-GCM, AES-256-GCM), nested `<sources>` with
  `<url-data>` source refs.

Integration suites added:
`tests/xep0369_mix_core.rs`, `xep0405_mix_pam.rs`,
`xep0407_mix_misc.rs`, `xep0430_inbox.rs`, `xep0448_encrypted_sfs.rs`,
and refreshed `xep0107_pep_mood.rs`, `xep0108_pep_activity.rs`,
`xep0118_pep_tune.rs` suites.

## Remaining work

- [ ] Wire MIX channel registry through `AppState` / dispatcher
      (`src/protocol/dispatch.rs`, `src/connection.rs`) so MIX IQs are
      routed to live sessions. Scaffold exists; dispatch is pending.
- [ ] Add `mix.<domain>` to disco items and advertise
      `mix_disco_features()` at the subdomain.
- [ ] Extend MAM to archive MIX messages with
      `message_type="mix"`; add a MIX-scoped archive query path.
- [ ] Add `mix_subscriptions` table migration + libSQL-backed
      `InboxStorage` impl in `waddle-server`.
- [ ] Remove MUC (`src/muc/`, MUC-tagged XEPs and tests) in a dedicated
      follow-up phase once clients migrate. Scaffold is additive today;
      MUC paths remain functional until that phase lands.
- [ ] Migrate web chat (`chat/src/lib/xmpp/`) and Apple client
      (`apps/apple/`) to MIX channel JIDs (`<ch>@mix.<domain>`).
- [ ] Expand XEP-0292 (vCard4) PEP publish/retrieve integration tests
- [ ] Add XEP-0402 (Bookmarks) PEP integration tests
- [ ] Add XEP-0115 (Entity Capabilities) integration tests
- [ ] Add XEP-0047 (In-Band Bytestreams) IQ session integration tests
