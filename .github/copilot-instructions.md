# Copilot Review Instructions

Review Waddle primarily for XMPP protocol correctness. Prioritize findings for non-XMPP codepaths, out-of-band behavior, incomplete protocol work, and deviations from RFCs or implemented XEPs.

## Priorities

1. Reject non-XMPP behavior. Waddle is XMPP-native; do not accept REST, GraphQL, WebSocket, database, local mock, worker, or proprietary side-channel paths for chat, presence, roster, rooms, identity, capabilities, delivery, archives, moderation, or other behavior that belongs in XMPP stanzas or XEP flows.
2. Reject stringly typed protocol payloads. XMPP data must flow through typed Rust values such as `crate::connection::Stanza`, `xmpp_parsers::{Iq, Message, Presence}`, `minidom::Element`, and `jid::{Jid, BareJid, FullJid}`. Do not carry structured protocol data as `String`, `&str`, or `Vec<u8>` except at the transport I/O boundary.
3. Reject ad-hoc XML. Build XML/XMPP payloads with typed structs or XML builders, then serialize. Do not accept `format!`, string concatenation, or `println!` for XML construction.
4. Check touched or advertised XEPs for conformance. If a PR changes namespaces, stanza shape, disco, capabilities, delivery, archives, pubsub, MUC, reactions, replies, receipts, moderation, or other XMPP behavior, compare it with the relevant XEP/RFC.
5. Require dedicated XEP tests. Any PR adding, changing, or advertising XEP behavior must add or update that XEP's Rust custom test suite.
6. Reject half-baked implementations: placeholders, TODO-backed behavior, stubs, UI-only flows without real XMPP/server behavior, advertised capabilities without implementation, or tests that only validate mocks while the real XMPP path is incomplete.
7. Reject legacy paths and backwards compatibility layers. Assume no production users or data. Do not accept shims, legacy aliases, fallback protocol paths, migration layers, old and new implementations side by side, or backwards-compatible behavior unless explicitly requested.

## XEP Conformance

For PRs touching XMPP behavior, clone or update the XSF XEP repo and use it as source of truth:

```bash
git clone https://github.com/xsf/xeps.git /tmp/xsf-xeps
git -C /tmp/xsf-xeps pull --ff-only
```

Inspect relevant `/tmp/xsf-xeps/xep-*.xml` files. Cite the XEP/RFC, affected file and line, what the spec requires, and what the PR does instead.

## Flag

- Non-XMPP APIs for message send/edit/delete, receipts, reactions, replies, presence, membership, room state, archives, search, discovery, capabilities, push, moderation, or identity.
- HTTP, GraphQL, RPC, database, worker, mock, or frontend-only state that bypasses stanza handling.
- Namespaces, element names, attributes, required children, stanza types, disco features, or error flows that do not match the relevant RFC or XEP.
- Advertised XEP support in disco, docs, types, UI, config, or tests without implementation and dedicated tests.
- Partial XEP implementations that only handle the happy path and omit required errors, discovery, validation, persistence, or interoperability semantics.
- Temporary, legacy, fallback, compatibility, or migration code kept beside the intended XMPP-native path.
- String literals for namespaces or XEP identifiers at call sites instead of constants or typed enums.
- Raw XML or storage rows parsed late instead of converted to typed values at the boundary.
- Human-readable string errors used as structured protocol errors.

Lead with protocol findings. Avoid broad style or design comments unless they affect XMPP-native behavior or XEP conformance.
