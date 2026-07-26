# Is end-to-end encryption viable for Waddle's LiveKit calls?

Research for waddle-social/waddle#1494 (wayfinder research, part of map issue #1489 — not edited by this document).
Date: 2026-07-26.

## TL;DR verdict

- **LiveKit's client-side E2EE is real, shipped, and cross-platform** (JS/web, Swift, Kotlin, Flutter, React Native, Python, Rust, Node, C++). It works by encrypting already-encoded media frames in the browser/client before they hit the WebRTC send path (WebRTC Insertable Streams / Encoded Transforms), so the LiveKit SFU only ever forwards opaque ciphertext.
- **The hard conflict is real and unavoidable**: turning on E2EE makes server-side egress/recording, live transcription, and (per LiveKit's own docs) most server-side media processing impossible, because the SFU never has the plaintext or the key. This is a straight trade-off, not an engineering gap LiveKit can close later — it's inherent to the architecture LiveKit chose (keys never leave clients, LiveKit "does not and cannot" hold them).
- **There is no clean, conformant XEP shape for LiveKit's key-provider model today.** LiveKit wants one rotating shared (or per-participant) symmetric secret for a room, re-keyed on membership change, handed to every participant out-of-band from the SFU. XEP-0384 (OMEMO) is pairwise device-to-device, not a room-key primitive; XEP-0396 (JET-OMEMO) is Deferred, over ten years without an editor, and designed for one-to-one Jingle file transfer key transport, not multiparty/mixer topologies; MUJI (XEP-0272) has no encryption companion XEP at all, and Waddle's own MUJI usage already funnels through a single non-conformant custom transport (`urn:waddle:transports:livekit:0`) to the SFU. Any group-rekeying scheme for a LiveKit shared secret would need a new `urn:waddle:*` namespace, stretching XEP-0384's per-device transport as the delivery channel at best. This is a genuine, honestly-reported gap, not something achievable by "just following the right XEP."
- **Recommendation shape**: (a) status quo (no E2EE) preserves everything Waddle currently offers server-side; (b) opt-in E2EE per call is buildable but is non-conformant custom-namespace work and permanently disables egress/recording/transcription for that call; (c) E2EE-by-default is the same trade-off made permanent and product-wide. See Options & Trade-offs below.

---

## Part 1 — LiveKit E2EE

### 1.1 Mechanism

LiveKit's E2EE is built on the **WebRTC Insertable Streams API**, standardized as **Encoded Transforms** (`RTCRtpScriptTransform` / `RTCRtpSender.transform` / `RTCRtpReceiver.transform`). This lets application JavaScript intercept already-encoded (post-codec) RTP frames on their way out to the network and on their way in from the network, and run arbitrary transform code (encryption/decryption) on them inside a Web Worker, before/after the browser's native WebRTC pipeline touches them.
Source: MDN, "Using WebRTC Encoded Transforms" — https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API/Using_Encoded_Transforms ("Baseline 2025 — Newly available. Since October 2025, this feature works across the latest devices and browser versions.")

LiveKit's JS SDK (`livekit/client-sdk-js`) implements this in `src/e2ee/`:
- `src/e2ee/worker/FrameCryptor.ts` — the actual per-track encryptor/decryptor. Header comment: "code inspired by https://github.com/webrtc/samples/blob/gh-pages/src/content/insertable-streams/endtoend-encryption/js/worker.js". Class `FrameCryptor` runs inside the worker; `encodeFunction`/`decodeFunction` operate on `RTCEncodedVideoFrame | RTCEncodedAudioFrame`.
  https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/worker/FrameCryptor.ts
- `src/e2ee/E2eeManager.ts` — the `E2EEManager`/`BaseE2EEManager` orchestration layer: wires the worker up to `Room`/`RTCEngine`, decides per-participant whether encryption is enabled, and gates on `isE2EESupported()` / `isScriptTransformSupportedForWorker()` / `isSafariBased()`.
  https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/E2eeManager.ts
- `src/e2ee/KeyProvider.ts` — `BaseKeyProvider` (the base class `ExternalE2EEKeyProvider` and the JS-SDK's shared-key provider both extend). Exposes `ratchetKey()`, which emits a `KeyProviderEvent.RatchetRequest`; the SDK marks the class `@experimental`.
  https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/KeyProvider.ts
- `src/e2ee/worker/ParticipantKeyHandler.ts` — per-participant key ring + ratchet implementation. `ratchetKey()` calls `ratchet(currentMaterial, this.keyProviderOptions.ratchetSalt)` to derive a new chain key from the current one (a KDF-style one-way ratchet, not a Double-Ratchet/DH ratchet — there's no per-message DH step here, just repeated HKDF-style derivation from a shared secret), and can auto-ratchet on decryption failure.
  https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/worker/ParticipantKeyHandler.ts

Frames are encrypted with **AES-GCM** using a key material derived from the shared/participant secret (confirmed independently by LiveKit's own Rust "Portal" SDK docs, which describe the underlying primitive as "AES-GCM with a shared secret. Both peers supply the same key before connecting." — https://github.com/livekit/portal/blob/main/docs/08-e2ee.md). This is the WebRTC-community "Insertable Streams E2EE" pattern popularized by Google Meet/Jitsi, informally SFrame-like (per-frame AEAD with a frame counter/IV), though LiveKit's implementation is its own, not a literal IETF SFrame (draft-ietf-sframe) implementation.

**Public API surface** (from LiveKit docs, https://docs.livekit.io/transport/encryption/ and https://docs.livekit.io/transport/encryption/start/ — the current canonical doc location; the originally-suggested `/home/client/tracks/encryption/` URL now redirects there):
- `ExternalE2EEKeyProvider` (JS/TS) / `BaseKeyProvider` (other SDKs) — you construct one, set a shared key or per-participant keys, pass it into `E2EEOptions`, and pass `E2EEOptions` into room/track setup.
- Support for **either a single shared key for the whole room, or unique keys per participant**.
- Key rotation ("ratcheting") requires extending `BaseKeyProvider`, overriding `onKeyRatcheted()`, and calling `ratchetKey()` yourself when you want to rotate (e.g., on membership change) — LiveKit explicitly leaves rekey-on-membership-change as an application responsibility, it does not automate it.
- Explicit statement of responsibility: "It is your responsibility to securely generate, store, and distribute encryption keys to your application at runtime. LiveKit does not (and cannot) store or transport encryption keys for you." — https://docs.livekit.io/transport/encryption/
- Explicit scope limit: "Signaling messages (control messages used to coordinate a WebRTC session) and API calls are not end-to-end encrypted — they're encrypted in transit using TLS, but the LiveKit server can still read them." — same page.
- The older `e2ee` field/API is deprecated in favor of a newer `encryption` field (`RoomOptions.encryption`), which also newly covers data channels (text/byte streams), not just media. Same page.

### 1.2 Browser / SDK support matrix (2026)

**Browser (Encoded Transforms / Insertable Streams) support:**
- Chrome: shipped a non-standard predecessor since Chrome 86; the standards-track `RTCRtpScriptTransform` is supported in current Chrome.
- Firefox: shipped `RTCRtpScriptTransform` in Firefox 117 (Mozilla intent-to-ship: https://groups.google.com/a/mozilla.org/g/dev-platform/c/Gowr5Fx5jng; tracking bug https://bugzilla.mozilla.org/show_bug.cgi?id=1631263).
- Safari: historically **unsupported** (tracked in WebKit bug 241124, "Support Insertable Streams/MediaStreamTrackProcessor on Safari, iOS and macOS," filed May 2022 with Microsoft/Zoom/Agora chiming in on its importance). **That bug is now RESOLVED (CONFIGURATION CHANGED)** — per the final comment from Apple's Youenn Fablet (2024-08-21): "This is enabled by default in the latest Safari betas as well as Safari Tech Preview," implemented by enabling the `MediaStreamTrackProcessingEnabled` flag on Cocoa platforms (dependency bug #268074, FIXED). Combined with the general Encoded Transforms "Baseline 2025 — Newly available. Since October 2025" note on MDN, this means: **as of 2026, Chrome, Firefox, and current Safari (macOS/iOS) all support the underlying browser API**, but older Safari versions in the field (anything materially predating the 2024-25 rollout) do not, and this is the newest/least-battle-tested of the three across the ecosystem.
  Sources: https://bugs.webkit.org/show_bug.cgi?id=241124, https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API/Using_Encoded_Transforms
- LiveKit's own JS SDK ships an `isInsertableStreamSupported()` runtime capability check (https://docs.livekit.io/reference/client-sdk-js/functions/isInsertableStreamSupported.html) and `E2eeManager.ts` explicitly branches on `isSafariBased()` / `isScriptTransformSupportedForWorker()` — i.e. LiveKit itself still treats Safari/webkit as a special case requiring feature-detection and codec/simulcast restrictions (see 1.4), not a browser it can assume full parity on.

**LiveKit SDK support for E2EE:** per https://docs.livekit.io/transport/encryption/start/, first-class E2EE key-provider APIs exist for:
- JavaScript/TypeScript (`ExternalE2EEKeyProvider`, current client-sdk-js version 2.21.0 per its `package.json` — https://github.com/livekit/client-sdk-js/blob/main/package.json)
- Swift/iOS (`BaseKeyProvider(isSharedKey:sharedKey:)` inside `E2EEOptions`)
- Android/Kotlin (`BaseKeyProvider()` + `setSharedKey()`)
- Flutter (`BaseKeyProvider`, shared key configuration)
- React Native (`BaseKeyProvider` plus a convenience `useRNE2EEManager` hook)
- Python, Rust, Node.js, C++ server/agent SDKs (shared-key patterns via `E2EEOptions`/`KeyProviderOptions`)

So E2EE is not a JS-only prototype feature — it is implemented consistently across every SDK Waddle would plausibly use (web `chat/`, and any future native Apple client per the `apple/` scope hinted at in commit-message conventions).

### 1.3 Performance cost

LiveKit's own docs (as fetched) do not publish a quantified CPU-overhead benchmark table for E2EE; the closest documented signal is architectural: encryption/decryption runs per-frame, per-track, inside a **Web Worker** (`e2ee.worker.ts`), which keeps it off the main thread but still means every encoded video/audio frame takes an extra AES-GCM encrypt/decrypt pass plus a structured-clone/postMessage hop into the worker for every frame. No public LiveKit benchmark numbers were found in the fetched docs or changelog; this should be treated as an open verification item (a computer-use / real-device CPU profile against a LiveKit E2EE call) rather than assumed free, especially on lower-power mobile devices given Waddle would enable this per participant per track.

### 1.4 Hard conflicts (what breaks with E2EE on)

- **Server-side egress/recording**: Not possible. Frames are encrypted client-side before ever reaching the SFU; the SFU only relays ciphertext and never holds a key. Multiple sources converge on this: LiveKit's docs statement "LiveKit does not (and cannot) store or transport encryption keys for you," and: "With E2EE enabled, media is encrypted by the LiveKit SDK before it leaves the client, and LiveKit servers see only opaque bytes. This fundamental limitation means that any server-side processing of media — including recording — is not possible without access to the encryption keys." There is no supported "give the SFU the key so it can also record" mode in the docs consulted.
- **Live transcription**: Same root cause — the server (or any bot/agent participant that isn't given the room key out-of-band) cannot see raw audio. A LiveKit `agents` GitHub issue confirms adjacent gaps exist even at the data-channel layer today (https://github.com/livekit/agents/issues/4991, "E2EE does not cover data tracks" — noting E2EE's data-channel coverage has had its own rough edges independent of the media-track story).
- **SIP integration**: Not directly documented in the fetched pages, but follows the same logic as recording — a SIP trunk gateway is architecturally a server-side media relay/transcoder and has no route to the E2EE room key unless explicitly given one; no LiveKit doc describes an E2EE+SIP compatibility path.
- **Simulcast**: Has real, documented friction, not a clean "just works." From the `client-sdk-js` CHANGELOG (https://github.com/livekit/client-sdk-js/blob/main/CHANGELOG.md):
  - "Prevent backup codec publishing when e2ee is enabled" (PR #943) and "Make sure no backup codecs are published when e2ee is enabled" (PR #941) — i.e. **multi-codec simulcast backup-codec publishing is disabled outright when E2EE is on**.
  - "Allow simulcast together with E2EE for supported Safari versions" (PR #1117) — phrased as a fix, implying simulcast+E2EE was previously broken/disallowed on Safari and had to be explicitly re-enabled once feature detection matured; the same changelog entry also fixes "the simulcast behaviour for iOS Chrome prior to 17.2," underscoring how fragile the Safari/iOS matrix has been here.
  - "Fix transceiver reuse for e2ee" (PR #1041), "Verify participant identity matching when unsetting transformer for e2ee" (PR #1032), "Ensure priority isn't set on all simulcast layers when using Firefox on iOS" (PR #1920, general simulcast, not E2EE-specific but same fragile-cross-browser-simulcast theme) — a pattern of ongoing transceiver/simulcast-adjacent bugs specifically in the E2EE code path.
  - Net read: standard simulcast (same codec, multiple spatial/temporal layers) works with E2EE today on modern browsers, because encoding happens before the encrypt step and simulcast layers are just multiple encoded outputs of the same encoder pipeline each independently encrypted — but **multi-codec backup-codec simulcast (the VP8/H264 dual-publish fallback LiveKit uses for older-device compatibility) is deliberately disabled under E2EE**, and Safari-specific simulcast+E2EE bugs have been a recurring source of point fixes as recently as PR #1117.

---

## Part 2 — XMPP-native key distribution

Cloned XEPs into `/Users/oyr/projects/waddle/.claude/worktrees/agent-a538c4bd947d78fdf/xeps` (shallow clone of https://github.com/xsf/xeps) per repo convention (root `CLAUDE.md`: "If ./xeps doesn't exist, clone xsf/xeps into ./xeps").

### 2.1 XEP-0384 (OMEMO Encryption)

File: `xeps/xep-0384.xml`. Status: **Experimental** (`<status>Experimental</status>`, header). Latest revision 0.9.1, 2026-04-06.

- OMEMO is built on the Double Ratchet + X3DH (`xeps/xep-0384.xml` lines 218–224, 299–332): "This XEP defines a protocol that leverages the Double Ratchet encryption scheme to provide multi-end to multi-end encryption... The general idea behind this protocol is to maintain separate, long-standing Double Ratchet-encrypted sessions with each device of each contact... a fresh, randomly generated encryption key [is used per message]. An encrypted header is added to the message for each device that is supposed to receive it." (lines 226–239)
- **There is no shared/room key concept in OMEMO.** The "Group Chats" section (lines 646–709) is explicit: OMEMO group-chat support is purely "N pairwise encryptions of the same payload key" — a participant "MUST first retrieve the members list and then fetch the device list for each member... and then subsequently fetch all bundles referenced by the device lists" and the message header contains "multiple `<keys>` elements. One for each participant of the room" (line 683), each independently wrapping the *same* per-message payload key, encrypted per-recipient-device via that device's individual Double Ratchet session. There is no single symmetric "room key" that all participants share and that could be handed to a media server or SFU as one artifact — the message-level payload key is still N-times pairwise-wrapped, and it's a fresh per-message key, not a stable rotating room secret.
- This matters directly for the LiveKit key-provider model: LiveKit wants **one (or few) rotating symmetric keys good for many frames across a session**, distributed once (or on ratchet/rekey events), not a fresh pairwise-wrapped key per XMPP stanza. OMEMO's model is architecturally the opposite of what LiveKit's `KeyProviderOptions`/`ExternalE2EEKeyProvider` expects. OMEMO *could* be used as a transport to deliver an initial shared secret to each device (pairwise, N times) but has no native concept of "the current room key" as a single referenceable value, and no native re-key-on-membership-change primitive beyond redoing that N-way pairwise send.
- Late joiners / member removal: OMEMO's group-chat section requires MUC members-only + non-anonymous config (line 648) and re-fetching devices/bundles on membership change, which is workable for chat, but it does not define any "evict device X from the shared secret" operation — because there is no shared secret to evict from. A late joiner or removed member story for a LiveKit room key would have to be built entirely on top of OMEMO, not derived from it.

### 2.2 XEP-0391 (Jingle Encrypted Transports, JET) and XEP-0396 (JET — OMEMO)

- XEP-0391: `xeps/xep-0391.xml`. Status: **Deferred** (line 14). Last substantive revision 0.1.2, 2018-07-31 — dormant for 8 years. JET's model (lines 100–107): one party generates a Transport Key + IV, encrypts that Transport Secret using an *already-established, ideally-authenticated* pairwise E2E session with the other party, and embeds the resulting Envelope Element in the Jingle `session-initiate`. This is explicitly **1:1, session-scoped**: "Prior to the Jingle session initiation, an already existing, established and (ideally) authenticated end-to-end encryption session between Romeo and Juliet MUST exist." (line 101). There is no multiparty/group concept anywhere in JET.
- XEP-0396 (JET-OMEMO): `xeps/xep-0396.xml`. Status: **Deferred** (line 14), with an explicit "Defer due to lack of activity" revision remark dated 2018-12-06 (lines 38–42) — i.e. the XSF formally shelved it for inactivity, not for any technical objection. It maps JET's Transport Key/IV onto OMEMO's existing "KeyTransportElement" (an OMEMO message with header-only, no payload — `xeps/xep-0396.xml` lines 57, 60). Its own "Limitations" section (lines 62–65) states: "Since OMEMO deviceIds are not bound to XMPP resources, the initiator MUST encrypt the Transport Key for every device of the recipient" — i.e. still N-way pairwise per recipient, same fan-out problem as OMEMO itself, just for a session key instead of a per-message key. **This is exactly the shape the task description hypothesized LiveKit's insertable-streams model might map onto — and it does not, cleanly**: XEP-0396 is scoped to secure a single Jingle *file-transfer* transport between exactly two parties (its `dependencies` are XEP-0391, XEP-0234 file-transfer, and XEP-0384 — no MUC/MIX/multiparty dependency at all), it has been Deferred for ~8 years with a single "lack of activity" note, and even its core mechanism (N-way pairwise Transport Key delivery) does not solve "one rotating shared room secret" — it solves "one Transport Key delivered redundantly to N devices of one recipient," not "one Transport Key shared identically by M participants and re-keyed when the group changes."
- Practical read: neither XEP-0391 nor XEP-0396 is a fit for the LiveKit room-key model as specified. Both are 1:1-session-secret XEPs; extending either to multiparty group re-keying would be a from-scratch protocol design exercise wearing JET's clothing, not an implementation of an existing conformant shape.

### 2.3 MUJI (XEP-0272) and encryption companions

- `xeps/xep-0272.xml`. Status: **Experimental** (line 16). Latest revision 0.2.0, 2024-08-21 (adds XEP-0482 call-invite support, real-JID Jingle routing).
- A directory listing of `xeps/` for anything MUJI-encryption-adjacent turned up **nothing**: `xeps/xep-0396.xml` (JET-OMEMO) has no MUC/MUJI dependency, and there is no `xep-0272`-companion encryption XEP in the local xsf/xeps checkout. MUJI itself has no encryption section at all — it defines only session/content/presence negotiation for multiparty Jingle inside a MUC (joining/leaving, content negotiation, relays/mixers, XEP-0482 call invites), with zero mention of key material, encryption, or E2EE anywhere in the document.
- MUJI explicitly anticipates centralized media relays/mixers ("Relays and Mixers" section, lines 289–311): "an RTP relay which is able to relay the stream to multiple participants... a mixer... can be used, which receives the media streams from other participants and mixes them" — this is precisely the LiveKit SFU topology Waddle already uses, and the XEP is silent on how such a relay/mixer would interact with any encryption layer, E2EE or otherwise.
- **Conclusion: there is no XEP-defined shape for "encrypted MUJI"** — confirming the task's hypothesis that this is likely a genuine standards gap, not something to be found by reading more carefully.

### 2.4 Waddle's current implementation (grounding)

`server/crates/waddle-xmpp-client-ffi/src/muji.rs` (module header, lines 1–16):
> "XEP-0272 Muji group-call exports... Waddle's divergence from vanilla XEP-0272 (sanctioned by its §'Relays and Mixers'): ONE Jingle session to the SFU mixer component `calls.<server-domain>` instead of a full mesh, with the custom `urn:waddle:transports:livekit:0` transport the server rewrites into LiveKit join credentials. The presence protocol (preparing → contents → bare-presence leave) stays XEP-conformant."

Key facts grounded in code:
- Waddle already uses **one Jingle session per participant to a single SFU mixer JID** (`muji_mixer_jid()`, `muji.rs:35-37`: `format!("calls.{server_domain}")`), not the MUJI-spec full mesh of pairwise Jingle sessions between all participants. This is explicitly called out as a sanctioned divergence under MUJI's own "Relays and Mixers" section, not a XEP violation per se — but it does mean any encryption story is between one client and the SFU mixer, not client-to-client, unless E2EE is layered independently on top (which is exactly LiveKit's model: SFU-oblivious client-to-client key material, unrelated to the Jingle signaling path).
- The join/session-initiate carries a **custom namespace transport**: `urn:waddle:transports:livekit:0` (confirmed in `server/crates/waddle-xmpp-client-ffi/src/types.rs:886`, `server/crates/waddle-xmpp-client-ffi/src/convert.rs:1194-1206`, and the test constant `NS_LIVEKIT_TRANSPORT` in `server/crates/waddle-xmpp-client-ffi/src/muji_tests.rs:20`) that the server rewrites into LiveKit join credentials/tokens (the `<token>` element at `convert.rs:1206`).
- Test coverage for the existing MUJI/Jingle-call surface lives in: `server/crates/waddle-xmpp-client-ffi/src/muji_tests.rs`, `server/crates/waddle-xmpp/tests/xep0272_muji.rs`, `server/crates/waddle-xmpp/tests/xep_waddle_in_call.rs`, `server/crates/waddle-server/tests/xep_waddle_in_call_ws.rs`, `server/crates/waddle-server/tests/calls_e2e_ws.rs`, `server/crates/waddle-server/src/server/routes/websocket/tests/muc.rs`, `server/crates/waddle-xmpp-core/src/disco/info/tests.rs`, `server/crates/waddle-xmpp-client/src/messaging/call/tests.rs` — confirming the "every implemented XEP needs a dedicated Rust test suite" rule is already being followed for MUJI/`urn:waddle:in-call:0`, which is the same bar any E2EE key-distribution addition would need to clear.
- Waddle's join/leave presence protocol stays XEP-0272-conformant (`preparing` → active contents → bare-presence leave, `muji.rs:59-87`); the divergence is entirely in the transport/session topology, which is already precedent for "custom `urn:waddle:*` namespace because no suitable XEP-defined shape exists" (the exact bar the root `CLAUDE.md` XEP-conformance rule sets for when a custom namespace is legitimate). A LiveKit-E2EE key-distribution mechanism would be squarely in the same category: no suitable XEP shape exists (per 2.1–2.3 above), so a custom namespace would be the conformant way to add one, not a rule violation.

---

## Options & trade-offs

### (a) No E2EE (status quo)
- **Effort**: zero — this is the current state.
- **What's preserved**: server-side egress/recording, live transcription, SIP integration, full simulcast (including multi-codec backup-codec fallback) all continue to work exactly as today.
- **What's lost**: no protection against a compromised/malicious server operator or infra breach reading call media in transit through the SFU (TLS/DTLS-SRTP still protects it from network attackers, per LiveKit's standard transport encryption — just not from LiveKit-the-service itself). This is the honest current posture; consistent with "Assume no production servers/users/data for this project" not being an argument either way here since this is about live production posture going forward.
- **UX impact**: none; no new toggle, no new failure modes.

### (b) Opt-in E2EE without recording (per-call toggle)
- **Effort**: substantial, and only partly on well-trodden ground. On the LiveKit side: wire `ExternalE2EEKeyProvider`/`E2EEOptions` into the `chat/` web client and any native client, disable egress/recording/transcription/SIP whenever the toggle is on (product-level gate, not something LiveKit does automatically), and handle the simulcast backup-codec restriction (already automatic in the SDK) plus Safari-version feature-detection (`isInsertableStreamSupported()`) with a graceful non-E2EE-capable-participant story. On the XMPP side: **there is no existing XEP-conformant shape to reach for** (2.1–2.3) — this would require a new `urn:waddle:*` namespace for room-key distribution and re-keying, carried most plausibly as a MUJI-adjacent presence/IQ extension analogous to how `urn:waddle:transports:livekit:0` already rides inside XEP-0272 signaling. Per the repo's own hard rule, this is legitimate ("use custom namespaces only when no suitable XEP-defined shape exists") — but it must be stated plainly in the PR/ADR that it is **not** XEP-0396/OMEMO conformant encryption, it is a bespoke Waddle protocol for a problem XEPs don't currently solve, and it would need its own dedicated Rust test suite per the XEP-custom-test-suite rule (treating it as if it were a XEP for governance purposes).
- **What breaks (only for E2EE-enabled calls)**: egress/recording, live transcription, SIP bridging, multi-codec simulcast fallback (all already forced off in this mode) — all scoped to just the calls where the toggle is on, so non-E2EE calls keep full functionality.
- **UX impact**: a call-level trust/feature trade-off surfaced to users ("this call cannot be recorded/transcribed while E2EE is on"), plus edge cases: what happens when a participant's client doesn't support Insertable Streams (older Safari, non-browser clients) — either exclude them or silently fall back to non-E2EE, both of which need explicit product decisions. Late-joiner and membership-change rekeying (a hard requirement per the task) has no existing XEP primitive to lean on, so this is where most of the actual engineering risk concentrates, not the LiveKit side.

### (c) E2EE by default (always on)
- **Effort**: same protocol/engineering work as (b), but no "off" path to fall back to — meaning the Safari-support-gap and non-browser-client questions from (b) become hard product blockers rather than opt-in edge cases, and the custom `urn:waddle:*` key-distribution namespace becomes load-bearing for every single call rather than an optional path.
- **What's lost, permanently, product-wide**: egress/recording, live transcription, SIP integration, multi-codec simulcast backup-codec fallback (bandwidth/compatibility cost on constrained devices) for every call, forever — these become features Waddle simply cannot offer at all, not features gated behind a toggle.
- **UX impact**: the largest of the three — no user can ever get a recorded/transcribed call, no SIP dial-in ever works, and any client (including any future integration, bot, or bridge) that can't do Insertable Streams is unconditionally excluded from calls. This is a legitimate privacy-maximalist stance, but it forecloses an entire category of product surface (this is the same trade-off Signal/WhatsApp calling makes, but Waddle is not those products and currently advertises richer call tooling).

## Sources

LiveKit:
- https://docs.livekit.io/transport/encryption/ (canonical E2EE overview; the task's suggested `/home/client/tracks/encryption/` URL redirects here)
- https://docs.livekit.io/transport/encryption/start/ (getting-started / API surface across SDKs)
- https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/E2eeManager.ts
- https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/KeyProvider.ts
- https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/worker/FrameCryptor.ts
- https://github.com/livekit/client-sdk-js/blob/main/src/e2ee/worker/ParticipantKeyHandler.ts
- https://github.com/livekit/client-sdk-js/blob/main/CHANGELOG.md
- https://github.com/livekit/client-sdk-js/blob/main/package.json (version 2.21.0)
- https://github.com/livekit/portal/blob/main/docs/08-e2ee.md (independent confirmation of AES-GCM shared-secret mechanism)
- https://github.com/livekit/agents/issues/4991 (E2EE data-channel gap)
- https://docs.livekit.io/reference/client-sdk-js/functions/isInsertableStreamSupported.html
- https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API/Using_Encoded_Transforms
- https://bugs.webkit.org/show_bug.cgi?id=241124 (Safari Insertable Streams support, RESOLVED/CONFIGURATION CHANGED)
- https://groups.google.com/a/mozilla.org/g/dev-platform/c/Gowr5Fx5jng and https://bugzilla.mozilla.org/show_bug.cgi?id=1631263 (Firefox RTCRtpScriptTransform)

XEPs (local checkout `xeps/`, cloned from https://github.com/xsf/xeps):
- `xeps/xep-0384.xml` — XEP-0384 OMEMO Encryption, status Experimental
- `xeps/xep-0391.xml` — XEP-0391 Jingle Encrypted Transports, status Deferred
- `xeps/xep-0396.xml` — XEP-0396 Jingle Encrypted Transports — OMEMO, status Deferred
- `xeps/xep-0272.xml` — XEP-0272 Multiparty Jingle (Muji), status Experimental
- `xeps/xep-0482.xml` — XEP-0482 Call Invites (referenced by MUJI 0.2.0, present locally, not itself encryption-relevant)

Waddle codebase:
- `server/crates/waddle-xmpp-client-ffi/src/muji.rs` (module doc comment, lines 1-16; `muji_mixer_jid`/`muji_mixer_target`, lines 35-48; presence builder, lines 59-87)
- `server/crates/waddle-xmpp-client-ffi/src/types.rs:886`
- `server/crates/waddle-xmpp-client-ffi/src/convert.rs:1194-1206`
- `server/crates/waddle-xmpp-client-ffi/src/muji_tests.rs:20`
- Test grounding: `server/crates/waddle-xmpp/tests/xep0272_muji.rs`, `server/crates/waddle-xmpp/tests/xep_waddle_in_call.rs`, `server/crates/waddle-server/tests/xep_waddle_in_call_ws.rs`, `server/crates/waddle-server/tests/calls_e2e_ws.rs`, `server/crates/waddle-server/src/server/routes/websocket/tests/muc.rs`, `server/crates/waddle-xmpp-core/src/disco/info/tests.rs`, `server/crates/waddle-xmpp-client/src/messaging/call/tests.rs`
