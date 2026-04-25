# TODO.md

## Test runtime snapshot (2026-04-19)

- `cargo test -p waddle-xmpp` (full suite): **~12s** (~1,580 tests)
- `cargo test -p waddle-xmpp --lib` (unit only): **~1.9s** (~1,546 tests)
- WebSocket C2S transport tests live under `crates/waddle-server/tests/xep*_ws.rs`.

All tests are native Rust — no Docker, CAAS, or external dependencies required.

## Recently landed — Social scaffold (2026-04-19)

Five social-feature XEPs landed server-side, all with dedicated integration
suites per the XEP test-suite rule in `CLAUDE.md`.

- Inbox (`src/inbox/`, `src/xep/xep0430.rs`) — in-memory `InboxView` +
  `InboxStorage` trait + `InMemoryInboxStorage`, protocol query /
  mark-read IQ, typed `ConversationKind { Direct, MucRoom }`.
- PEP Mood / Activity / Tune (`src/xep/xep0107.rs`, `0108.rs`,
  `0118.rs`) — fully typed payloads, retraction form, disco feature
  constants.
- Encrypted SFS (`src/xep/xep0448.rs`) — closed `Cipher` enum
  (AES-128-GCM, AES-256-GCM), nested `<sources>` with
  `<url-data>` source refs.

Integration suites added: `tests/xep0430_inbox.rs`,
`xep0448_encrypted_sfs.rs`, and refreshed `xep0107_pep_mood.rs`,
`xep0108_pep_activity.rs`, `xep0118_pep_tune.rs` suites.

MIX (XEP-0369 / 0405 / 0407) was investigated and dropped: the XEP series
is inactive (0403/0404/0406/0407/0408 Deferred), no major server or client
ships it (Conversations, Dino, Gajim, Prosody, ejabberd, Openfire), and
community consensus is MUC-with-extensions wins. Waddle stays on MUC.

## Remaining work

- [x] Wire `InboxStorage` into `AppState` with a libSQL-backed impl in
      `waddle-server` so inbox state survives restarts.
- [ ] Expand XEP-0292 (vCard4) PEP publish/retrieve integration tests
- [ ] Add XEP-0402 (Bookmarks) PEP integration tests
- [ ] Add XEP-0115 (Entity Capabilities) integration tests
- [ ] Add XEP-0047 (In-Band Bytestreams) IQ session integration tests
