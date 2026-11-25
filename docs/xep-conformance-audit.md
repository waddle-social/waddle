# XEP Conformance Audit

Tracking document for the cross-codebase audit of every XEP Waddle implements
or advertises. One row per XEP. Each gap becomes an isolated PR.

## Rules of the audit

- **Spec source**: `./xeps/xep-NNNN.xml` is authoritative.
- **Conformance hard rule** (from `CLAUDE.md`): if Waddle advertises an XEP
  feature or uses an official `urn:xmpp:*`, `jabber:*`, or
  `http://jabber.org/*` namespace, the wire shape and behavior MUST conform
  to that XEP exactly. Waddle-specific semantics live under `urn:waddle:*`.
- **Test hard rule** (from `CLAUDE.md`): every implemented XEP MUST have a
  dedicated Rust custom test suite. Audit findings that touch behavior MUST
  add/update that XEP's tests in the same PR.
- **Isolation**: one PR per finding (per non-conformance). Documentation-only
  updates to this file may be batched.

## How to read the table

- **Adv** = advertised in `disco_info` for at least one component
  (server / pubsub / spaces / MUC service / MUC room / PEP).
- **Impl** = has a dedicated source module under `server/crates/.../xep/`
  or `server/crates/waddle-xmpp-core/src/xep0NNN*`.
- **Tests** = has a dedicated test file `server/crates/.../tests/xep0NNN_*.rs`
  (a multi-XEP test file like `xep0054_0049_0191_ws.rs` counts only if the
  XEP has assertions of its own behavior — to be confirmed during audit).
- **Status**:
  - `unaudited` — not yet reviewed
  - `auditing` — review in progress
  - `conformant` — review complete, no gaps found
  - `gap` — review complete, gaps found (PR # linked)
  - `fixed` — gap PR merged
  - `not-implemented` — referenced for wire format only, not implementing the XEP

## Master inventory

| XEP  | Title                                                | Namespace                          | Adv | Impl | Tests | Status     | Notes / PR |
|------|------------------------------------------------------|------------------------------------|-----|------|-------|------------|------------|
| 0004 | Data Forms                                           | jabber:x:data                      |  -  |  Y   |   ?   | unaudited  | Building block, used by many. |
| 0012 | Last Activity                                        | jabber:iq:last                     |  Y  |  Y   |   Y   | fixed      | Last-activity queries require presence authorization (#2670). |
| 0030 | Service Discovery                                    | http://jabber.org/protocol/disco#* |  Y  |  Y   |   Y   | fixed      | disco/info.rs. Extension bots (`<plugin>@extensions.<domain>`, bare or `/bot`) answer as `client/bot` with no items; other extensions-domain addresses return item-not-found. A room's extension bots are listed under disco#items node `urn:waddle:room:bots:0` (empty for users who can't read the room). Any unhandled disco#items node returns item-not-found. Account disco#info now requires presence authorization and returns `service-unavailable` for missing or unauthorized accounts (#2667). Unknown server disco#info nodes return `item-not-found` (#2672). Evidence: `xep0030_account_disco_privacy_ws.rs`, `xep0030_unknown_node_ws.rs`. |
| 0045 | Multi-User Chat                                      | http://jabber.org/protocol/muc     |  Y  |  Y   |   Y   | unaudited  | muc_* feature family |
| 0047 | In-Band Bytestreams                                  | http://jabber.org/protocol/ibb     |  -  |  Y   |   -   | unaudited  | Impl exists, no advert? |
| 0048 | Bookmarks (legacy)                                   | storage:bookmarks                  |  -  |  Y   |   -   | fixed      | Superseded by 0402. Legacy Private XML bridge replaces the PEP bookmark set on set (dropped bookmarks are retracted) and returns `internal-server-error` on storage errors instead of success (#2674). |
| 0049 | Private XML Storage                                  | jabber:iq:private                  |  Y  |  Y   |   ?   | fixed      | Shared test xep0054_0049_0191. Legacy bookmark set now replaces instead of merging, and bridge storage errors surface (#2674). |
| 0050 | Ad-Hoc Commands                                      | http://jabber.org/protocol/commands|  Y  |  Y   |   -   | unaudited  | |
| 0054 | vcard-temp                                           | vcard-temp                         |  Y  |  Y   |   Y   | unaudited  | |
| 0059 | Result Set Management                                | http://jabber.org/protocol/rsm     |  -  |  Y   |   -   | unaudited  | Helper for MAM etc. |
| 0060 | Publish-Subscribe                                    | http://jabber.org/protocol/pubsub  |  Y  |  Y   |   Y   | unaudited  | Many sub-features advertised |
| 0065 | SOCKS5 Bytestreams                                   | http://jabber.org/protocol/bytestreams |  Y |  ?   |   -   | unaudited  | socks5_bytestreams in features |
| 0077 | In-Band Registration                                 | jabber:iq:register                 |  -  |  Y   |   -   | unaudited  | |
| 0080 | User Location                                        | http://jabber.org/protocol/geoloc  |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0082 | XMPP Date and Time Profiles                          | (profiles only)                    |  -  |  Y   |   -   | unaudited  | Profile usage, no advert |
| 0084 | User Avatar                                          | urn:xmpp:avatar:metadata           |  Y  |  Y   |   Y   | unaudited  | PEP node + notify; avatar nodes default `open` (not XEP-0163 `presence`) so non-roster peers resolve avatars; legacy `presence` avatar nodes without an owner-configure marker are reopened at startup/publish (pre-marker explicit `presence` choices are indistinguishable and are reset) |
| 0085 | Chat State Notifications                             | http://jabber.org/protocol/chatstates |  Y |  Y   |   Y   | fixed      | Web (#2676) and Apple (#2677) clients now have a setting to disable chat-state notifications (§ MUST). |
| 0092 | Software Version                                     | jabber:iq:version                  |  Y  |  Y   |   Y   | unaudited  | |
| 0106 | JID Escaping                                         | (no namespace, escape rules)       |  -  |  Y   |   -   | unaudited  | |
| 0107 | User Mood                                            | http://jabber.org/protocol/mood    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0108 | User Activity                                        | http://jabber.org/protocol/activity |  - |  Y   |   Y   | unaudited  | PEP node |
| 0115 | Entity Capabilities                                  | http://jabber.org/protocol/caps    |  Y  |  Y   |   Y   | fixed      | Disco#info identity `xml:lang` is read from the XML namespace so the verification hash matches (#2666). |
| 0118 | User Tune                                            | http://jabber.org/protocol/tune    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0153 | vCard-Based Avatars                                  | vcard-temp:x:update                |  -  |  Y   |   -   | unaudited  | |
| 0160 | Best Practices for Handling Offline Messages         | msgoffline                         |  Y  |  ?   |   Y   | unaudited  | |
| 0163 | Personal Eventing Protocol                           | http://jabber.org/protocol/pubsub#pep |  Y |  Y   |   Y   | fixed      | Last published item is sent to newly-available resources on presence (#2668). Contacts can read presence- and roster-model nodes they are authorized for (#2673). Notifications reach all of the owner's connected resources with `+notify`, including the publishing resource (#2684). |
| 0167 | Jingle RTP Sessions                                  | urn:xmpp:jingle:apps:rtp:1         |  Y  |  Y   |   -   | fixed      | Server `calls.<domain>` mixer advertises RTP features. Rust client (used by Apple/Android) advertises `urn:xmpp:jingle:1`, `urn:xmpp:jingle:apps:rtp:1` and the audio/video media types (#2681); no ICE-UDP/DTLS features, since media uses the LiveKit transport. |
| 0172 | User Nickname                                        | http://jabber.org/protocol/nick    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0184 | Message Delivery Receipts                            | urn:xmpp:receipts                  |  Y  |  Y   |   Y   | unaudited  | |
| 0191 | Blocking Command                                     | urn:xmpp:blocking                  |  Y  |  Y   |   Y   | unaudited  | |
| 0198 | Stream Management                                    | urn:xmpp:sm:3                      |  Y  |  Y   |   Y   | unaudited  | Credential lifecycle IQs (`urn:waddle:credential:1`) use idempotent replay nonces and opaque results/errors (`payload: None` on error) so `sm_unacked` never stores secret material. |
| 0199 | XMPP Ping                                            | urn:xmpp:ping                      |  Y  |  Y   |   Y   | unaudited  | |
| 0201 | Best Practices for Message Threads                   | (none, &lt;thread/&gt; element)            |  -  |  Y   |   Y   | gap        | Advertised `urn:xmpp:threads:0` (Waddle-fabricated). **PR #609** drops the advert; XEP-0201 wire behavior preserved + regression guard added. |
| 0202 | Entity Time                                          | urn:xmpp:time                      |  Y  |  Y   |   Y   | unaudited  | |
| 0203 | Delayed Delivery                                     | urn:xmpp:delay                     |  -  |  Y   |   Y   | unaudited  | |
| 0223 | Persistent Storage of Public Data via PubSub         | (profile of PEP)                   |  -  |  Y   |   -   | unaudited  | |
| 0237 | Roster Versioning                                    | urn:xmpp:features:rosterver        |  Y  |  Y   |   Y   | unaudited  | |
| 0249 | Direct MUC Invitations                               | jabber:x:conference                |  -  |  Y   |   -   | unaudited  | |
| 0277 | Microblogging over XMPP                              | urn:xmpp:microblog:0               |  -  |  ?   |   Y   | unaudited  | Test exists, impl path unclear |
| 0280 | Message Carbons                                      | urn:xmpp:carbons:2 + rules:0       |  Y  |  Y   |   Y   | unaudited  | |
| 0292 | vCard4 over XMPP                                     | urn:xmpp:vcard4 + +notify          |  Y  |  Y   |   Y   | fixed      | §6.1 canonical `access_model=open` wired through `NodeConfig::pep_for_node(PEP_NODE_VCARD4)` + reconcile-on-publish for legacy `presence`-access nodes in **PR #669**. Dedicated suite: `crates/waddle-xmpp/tests/xep0292_vcard4.rs` + `crates/waddle-server/tests/xep0292_vcard4_ws.rs`. `urn:xmpp:contacts` node defaults to `whitelist` (#2679). |
| 0297 | Stanza Forwarding                                    | urn:xmpp:forward:0                 |  -  |  Y   |   -   | unaudited  | Used by carbons / MAM |
| 0300 | Use of Cryptographic Hash Functions in XMPP          | urn:xmpp:hashes:2                  |  -  |  Y   |   -   | unaudited  | |
| 0308 | Last Message Correction                              | urn:xmpp:message-correct:0         |  Y  |  Y   |   Y   | unaudited  | |
| 0313 | Message Archive Management                           | urn:xmpp:mam:2 + :2#extended       |  Y  |  Y   |   Y   | unaudited  | Plus Waddle fulltext / thread extensions |
| 0317 | Hats                                                 | urn:xmpp:hats:0                    |  Y  |  Y   |   Y   | partial    | Authority stays in XEP-0045 (#611). The deployment assigns hats through `ServerHats`; every server-built occupant presence (join, late-joiner replay, role/affiliation change, leave) carries them, and client-sent hats are stripped. Today only extension bots wear one (`urn:waddle:hats:bot`). Hat list/assign commands (§3.2–3.6) are not implemented. Tests: `waddle-xmpp/tests/xep0317_hats.rs`. |
| 0319 | Last User Interaction in Presence                    | urn:xmpp:idle:1                    |  -  |  Y   |   -   | unaudited  | |
| 0333 | Chat Markers                                         | urn:xmpp:chat-markers:0            |  Y  |  Y   |   Y   | unaudited  | |
| 0334 | Message Processing Hints                             | urn:xmpp:hints                     |  -  |  Y   |   Y   | unaudited  | Hints are bypassed on bare/server JIDs, so credential lifecycle operations use `<iq/>` (`urn:waddle:credential:1`) rather than `<message/>`. |
| 0352 | Client State Indication                              | urn:xmpp:csi:0                     |  Y  |  ?   |   -   | unaudited  | Feature::csi exists, impl path? |
| 0357 | Push Notifications                                   | urn:xmpp:push:0                    |  Y  |  Y   |   Y   | unaudited  | |
| 0359 | Unique and Stable Stanza IDs                         | urn:xmpp:sid:0                     |  Y  |  Y   |   Y   | unaudited  | |
| 0363 | HTTP File Upload                                     | urn:xmpp:http:upload:0             |  Y  |  Y   |   -   | fixed      | Slot `<put/>` headers are limited to Authorization, Cookie and Expires; `Content-Type` is no longer sent (#2680). |
| 0372 | References                                           | urn:xmpp:reference:0               |  Y  |  Y   |   Y   | unaudited  | |
| 0377 | Spam Reporting                                       | urn:xmpp:reporting:1               |  -  |  Y   |   -   | unaudited  | |
| 0392 | Consistent Color Generation                          | (no namespace, algorithm)          |  -  |  Y   |   -   | unaudited  | |
| 0393 | Message Styling                                      | urn:xmpp:styling:0                 |  -  |  Y   |   -   | unaudited  | |
| 0401 | Easy User Onboarding                                 | urn:xmpp:invite                    |  -  |  Y   |   -   | unaudited  | |
| 0402 | PEP Native Bookmarks                                 | urn:xmpp:bookmarks:1               |  Y  |  Y   |   -   | fixed      | bookmarks2. `#compat-pep` is no longer advertised, since there is no PEP `storage:bookmarks` bridge (#2675). |
| 0410 | MUC Self-Ping (Schrödinger's Chat)                   | (uses ping, optimization advert)   |  Y  |  Y   |   -   | unaudited  | |
| 0421 | Anonymous unique occupant identifiers for MUCs       | urn:xmpp:occupant-id:0             |  Y  |  Y   |   Y   | unaudited  | |
| 0424 | Message Retraction                                   | urn:xmpp:message-retract:1         |  Y  |  Y   |   Y   | fixed      | Retraction tombstones the message and its XEP-0308 corrections in MAM, keeping `<replace/>` and copying the retraction `<reason/>` (#2671). |
| 0425 | Message Moderation                                   | urn:xmpp:message-moderate:1        |  Y  |  Y   |   Y   | unaudited  | |
| 0428 | Fallback Indication                                  | urn:xmpp:fallback:0                |  Y  |  Y   |   -   | unaudited  | |
| 0430 | Inbox                                                | urn:xmpp:inbox:1                   |  Y  |  Y   |   Y   | fixed      | Streaming `<message><entry/></message>` per conversation + closing `<iq type='result'><fin/></iq>` with `total`/`unread`/`all-unread`. Supports `unread-only` + `messages` filters, RSM `<max/>` paging, XEP-0333 displayed-marker → unread decrement. Mark-read remains a Waddle-private `urn:waddle:inbox:0` IQ-set until XEP-0333 covers 1:1 DMs end-to-end. ESL `erlang-solutions.com:xmpp:inbox:0` compat dropped (no active consumers). Tests: `server/crates/waddle-server/tests/xep0430_inbox_ws.rs`. PR #693. `<fin/>` `total`/`unread`/`all-unread` are inbox-wide, independent of filters and paging (#2678). |
| 0431 | -                                                    | (assigned)                         |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0433 | Extended Channel Search                              | urn:xmpp:channel-search:0          |  -  |  Y   |   Y   | fixed      | Search lists only public rooms (#2669). Tests: `crates/waddle-server/tests/xep0433_channel_search_ws.rs`, `crates/waddle-xmpp/tests/xep0433_extended_channel_search.rs`. |
| 0437 | Room Activity Indicators                             | urn:xmpp:rai:0                     |  -  |  Y   |   -   | unaudited  | |
| 0444 | Message Reactions                                    | urn:xmpp:reactions:0               |  Y  |  Y   |   Y   | unaudited  | |
| 0445 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0446 | File Metadata Element                                | urn:xmpp:file:metadata:0           |  -  |  Y   |   -   | unaudited  | |
| 0447 | Stateless File Sharing                               | urn:xmpp:sfs:0                     |  -  |  Y   |   -   | unaudited  | |
| 0448 | Encryption for stateless file sharing                | urn:xmpp:esfs:2                    |  -  |  Y   |   -   | fixed      | Encrypted shares require the plaintext `<size/>`: the client builder, FFI and WASM boundaries refuse to send one without it, and the parser ignores shares that lack it (#2682). |
| 0449 | Stickers                                             | urn:xmpp:stickers:0                |  -  |  Y   |   -   | unaudited  | |
| 0452 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0461 | Message Replies                                      | urn:xmpp:reply:0                   |  Y  |  Y   |   Y   | unaudited  | |
| 0469 | Bookmark Pinning                                     | urn:xmpp:bookmarks-pinning:0       |  -  |  Y   |   -   | unaudited  | |
| 0470 | Pubsub Attachments                                   | urn:xmpp:pubsub-attachments:1      |  -  |  Y   |   -   | unaudited  | |
| 0471 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0472 | Pubsub Social Feed                                   | urn:xmpp:pubsub-social-feed:0      |  -  |  Y   |   -   | unaudited  | |
| 0486 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0488 | Pinning Chat Messages                                | urn:xmpp:pin:0                     |  -  |  Y   |   -   | unaudited  | |
| 0490 | Message Displayed Synchronization                    | urn:xmpp:mds:displayed:0           |  Y  |  Y   |   Y   | fixed      | PR #665. Well-known PEP node with whitelist + max-items=max + send-last=never defaults; chat publishes alongside XEP-0333 displayed, catches up + subscribes on connect. Caps-driven `+notify` advertise pending chat caps emission. Server-assist (§3.5) out of scope. |
| 0492 | Chat Notification Settings                           | urn:xmpp:notification-settings:1   |  -  |  Y   |   Y   | partial    | Wire+projection in `xep0492_chat_notification_settings`; gate enforced at DM push fan-out via `PushDispatchDecision::evaluate` — covered by `xep0492_push_enforcement_ws` (matrix) + `tests/messages.rs::xep0492_direct_chat_*` (wire). MUC push fan-out is not yet wired (`QueueOfflineDelivery` only fires for DMs); reducer matrix locks the contract for when it lands. |
| 0500 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0501 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0502 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0503 | Spaces                                               | urn:xmpp:spaces:0                  |  Y  |  Y   |   Y   | unaudited  | Advert gated until owner subs done |
| 0508 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0513 | Explicit Mentions                                    | urn:xmpp:mentions:0                |  Y  |  Y   |   Y   | fixed      | Plus `#channel` profile. Mentions are carried as `<mention type target active/>` inside XEP-0394 `<markup><span start end>`; Waddle-only `jid`/`uri`/`noping` moved to a sibling `urn:waddle:mentions:0` element (#2683). |
| —    | (Waddle) Threads view                                | urn:waddle:threads:0               |  Y  |  Y   |   Y   | fixed      | Waddle-namespaced (no XEP equivalent — Fluux made the same call for their custom conversation list). Server reads `inbox_entries WHERE thread_id != ''`; chat client renders a two-section view (Unread / Following). PR #671. |
| —    | (Waddle) Call lifecycle reports                      | urn:waddle:call-lifecycle:0        |  Y  |  Y   |   Y   | fixed      | Closed-set client operational reports; counted and acknowledged without call-state changes. Dedicated `xep_waddle_call_lifecycle` and `calls_e2e_ws` coverage. |
| —    | (Waddle) Inference credential lifecycle              | urn:waddle:credential:1            |  Y  |  Y   |   Y   | fixed      | Server-domain IQ-only client-sealed credential lifecycle (subkey fetch, device enrollment, credential enroll/status/rotate/revoke, policy mutation); stripped recursively across all IQ error-bounce paths. Dedicated core + server integration suites. |

## Currently identified gaps (preliminary, pre-deep-audit)

### 1. `urn:xmpp:threads:0` advertised without spec backing

`Feature::threads()` in `server/crates/waddle-xmpp-core/src/disco/info/features.rs:86` advertises the namespace `urn:xmpp:threads:0`. A scan of `./xeps/xep-*.xml` and `./xeps/inbox/*.xml` finds no XEP or ProtoXEP that defines this namespace. XEP-0201 (Best Practices for Message Threads) uses the bare `<thread/>` element in messages and does not define a disco feature.

This violates the CLAUDE.md hard rule: "Do not use official XEP namespaces for Waddle-specific semantics."

Resolution options:
1. Drop the advertisement (matches XEP-0201 reality — no feature needed).
2. Move to `urn:waddle:threads:0` if the advert encodes Waddle-specific thread semantics that callers depend on.

Decision pending: check what consumers (clients, tests) read this advert before choosing.

## Waddle-namespaced extensions

Tracked separately from XEPs because they are intentionally Waddle-specific
and not derived from any XEP. Each entry must justify why no XEP shape
applies and what the wire commitment is.

### `urn:waddle:call-lifecycle:0`

- **Carrier**: IQ-set with `<report xmlns='urn:waddle:call-lifecycle:0'/>`,
  addressed to the reporting client's own server domain or with no `to`.
- **Args**: all six attributes are required: `kind` (`dm` | `muc`),
  `setup-outcome` (`proposed` | `accepted` | `declined` | `timeout` | `failed`),
  `end-reason` (`hangup` | `peer-left` | `error` | `reconnect-exhausted`),
  `duration` (`none` | `under-1m` | `1m-10m` | `10m-60m` | `over-60m`),
  `reconnects` (`none` | `once` | `multiple`), and `platform`
  (`ios` | `macos` | `android` | `web`). `accepted` means the call's media
  session connected on the reporting device. Missing attributes or unknown
  values of known attributes yield `bad-request`; unknown extra attributes
  and child elements are ignored for forward compatibility. There is no
  timestamp on the wire.
- **Result**: empty IQ-result after counting; IQ-get yields `bad-request`,
  and a foreign `to` reaching the handler yields `service-unavailable`.
  The server is stateless and does not deduplicate: each client sends each
  report at most once and never retries. Reports only produce counters,
  a structured log and an acknowledgement; they do not affect routing,
  call state or other stanzas.
- **ACL**: a bound authenticated session is required (`not-authorized`
  otherwise). A separate per-bare-JID sliding window permits 12 reports
  per 10 minutes; excess reports yield `policy-violation` with error type
  `cancel` and do not increment the client lifecycle counters.
- **Advert**: `Feature::waddle_call_lifecycle()` in `call_features()` only
  when call handlers are configured, never in the base `server_features()`.
- **Why a Waddle namespace?** No XEP carries a client's own view of a call.
  Jingle and XEP-0353 reasons are signalling the peer sees; they cannot say
  that media connected on this device.
- **Tests**: `server/crates/waddle-xmpp/tests/xep_waddle_call_lifecycle.rs`
  covers replies, rate limits and exported counters; core inline tests
  cover the typed wire shape, and `calls_e2e_ws` covers discovery and
  report acknowledgement over WebSocket.

### `urn:waddle:credential:1`

- **Carrier**: Authenticated `<iq type='get'|'set'>` addressed to the user's
  own server domain (or with `to` omitted). Never carried in `<message/>`
  (XEP-0334 hints are bypassed on bare/server JIDs) and never carried in
  XEP-0050 forms on `extensions.<domain>`.
- **Operations & Args**:
  - `<get-subkey/>` (`IQ-get`): fetches the broker's active rotatable P-256
    ECDH subkey bundle (`<subkey-bundle/>`) signed by the client-pinned Root
    Broker-Key-Signing Key (BKSK).
  - `<enroll-device/>` (`IQ-set`): enrolls a P-256 device signing key with
    mandatory independent IdP proof (`Colony` JWT `cnf.jkt` or WebAuthn) and
    proof-of-possession signature.
  - `<enroll-credential/>`, `<rotate-credential/>`, `<revoke-credential/>`
    (`IQ-set`): client-sealed (`ECDH-ES + HKDF-SHA256 + AES-256-GCM`)
    credential custody mutations with single-use XEP-0198 replay nonces.
  - `<credential-status/>` (`IQ-get`): lists opaque credential metadata only.
  - `<mutate-policy/>` (`IQ-set`): tenant/room inference policy mutation
    requiring broker-verified IdP admin proof.
- **Result & Error Opacity**: Results return only opaque identifiers, statuses,
  versions, and public keys. Every error returns `payload: None`. All four
  server IQ error-bounce paths (`route_to_connection`, `destination`,
  `full_jid_forward`, `transport_xml`/`muc_admin`) and XEP-0191/XEP-0363 error
  helpers recursively strip `urn:waddle:credential:*` and
  `urn:waddle:inference:credential:*` elements so `sm_unacked` stays secret-free.
- **ACL & Proof Enforcement**: Cell-local `native_users` SCRAM accounts are
  explicitly rejected (`forbidden`), and cell-asserted admin flags
  (`serverOwnerLocalparts`, `pubsub_affiliations`) are rejected without
  broker-verified IdP admin proof.
- **Advert**: `Feature::waddle_credential()` in `waddle-xmpp-core`, advertised
  only when broker custody is configured.
- **Why a Waddle namespace?** No XEP defines client-pinned root-signed broker
  subkeys, ECDH-ES sealed inference credential enrollment with XEP-0198
  idempotent nonces, or broker-custodied inference policy mutations.
- **Tests**: `waddle-xmpp-core::waddle_credential` unit tests,
  `waddle-broker` crypto/KMS/IdP/restore suites, and `waddle-server`
  integration tests (`credential_iq_contract`, `credential_iq_sm_replay`,
  `credential_iq_bounce_stripping`, `credential_iq_nonce_boundaries`,
  `credential_iq_malformed_xml`, `credential_iq_four_bounce_paths`,
  `credential_iq_bounce_m3`, `pairwise_sealed_enroll_bounce_strip`).

### `urn:waddle:admin:users:list:0` — Admin V1 Users panel

- **Carrier**: XEP-0050 ad-hoc command (`<command>` IQ-set against the
  server domain).
- **Args** (`<x type='submit'>`, `FORM_TYPE = urn:waddle:admin:users:list:0`):
  `prefix` (text-single, optional), `page_size` (text-single, default 50,
  capped 200), `after_cursor` (text-single, optional).
- **Result** (`<x type='result'>`, same `FORM_TYPE`): reported columns
  `jid` / `display_name` / `has_owner_hat`; one `<item/>` per user;
  optional top-level `next_cursor` text-single.
- **ACL**: server refuses non-owners (`server_owner_jids`) with
  `<forbidden/>`.
- **Advert**: `Feature::new("urn:waddle:admin:users:list:0")` on
  `server_features()` so clients can detect support without enumerating
  commands.
- **Why a Waddle namespace?** No XEP defines "list users with prefix
  search." XEP-0133 ("Service Administration") lists `Get User List`
  among its `urn:xmpp:adminstration:0` commands, but the spec is a
  fixed list keyed by command nodes with no prefix/seek-pagination
  semantics — the V1 chat panel needs a typed, cursor-paginated page
  to drive UI scroll. The custom node uses `urn:xmpp:waddle:*` so the
  cross-spec audit table doesn't shadow an official XEP namespace.
- **Tests**: `server/crates/waddle-server/tests/admin_users_list_command_ws.rs`
  covers owner-can-list / non-owner-forbidden / prefix-narrows /
  pagination-cursor-round-trips; unit tests under
  `server/crates/waddle-server/src/admin/users_list.rs::tests` cover
  arg parsing + result-form shape.

### `urn:waddle:admin:spaces:*` — Admin V2 Spaces panel (PR #685)

Six XEP-0050 ad-hoc commands keyed by the namespace URI (which doubles
as the `FORM_TYPE` for the args/result forms). Owner-gated via the same
`is_community_owner(state, requesting_jid)` chokepoint as Admin V1.

- `urn:waddle:admin:spaces:list:0` — paginated read; reports
  `space_jid`/`name`/`description`/`icon_url`/`channel_count`/`member_count`
  with optional `next_cursor`.
- `urn:waddle:admin:spaces:create:0` — args `name` (1–80) plus optional
  `description`/`icon_url`; mints a slugified space localpart, writes
  the metadata row + the backing PubSub node, seeds server-owner
  PubSub Owner affiliations.
- `urn:waddle:admin:spaces:update:0` — args `space_jid` + optional
  patch fields; rejects unknown space with `<item-not-found/>`.
- `urn:waddle:admin:spaces:delete:0` — args `space_jid` + `confirm` (must
  equal `"yes"`); cascade-destroys child MUC rooms via `DestroyRoom`,
  deletes the PubSub node and metadata row.
- `urn:waddle:admin:spaces:members:0` — paginated read of PubSub
  affiliations on the space node; collapses `Outcast` to "none" for the
  V2 spaces wire surface.
- `urn:waddle:admin:spaces:set-role:0` — args `space_jid`,
  `member_jid`, `role ∈ {owner,admin,member,none}`; projects onto
  XEP-0060 affiliations (`owner` ↔ `Owner`, `admin` ↔ `Publisher`,
  `member` ↔ `Member`, `none` removes the row).

**Why Waddle-namespaced?** No XEP defines "manage spaces"; XEP-0133 is
fixed and doesn't cover the cursor-paginated CRUD the admin panel
needs. XEP-0503 covers the space discovery surface only.

**Advert**: each URI is also pushed onto `server_features()` so chat
clients can detect Admin V2 support without enumerating commands.

**Tests**:
`server/crates/waddle-server/tests/admin_spaces_ws.rs` — 15 #[tokio::test]
cases exercising the wire surface end-to-end.
Unit tests under `server/crates/waddle-server/src/admin/spaces.rs::tests`
cover arg parsing, the role vocabulary, and the result-form shape.

### `urn:waddle:admin:channels:*` — Admin V2 Channels panel (PR #685)

Eight XEP-0050 ad-hoc commands; owner-gated identically to spaces.

- `urn:waddle:admin:channels:list:0` — walks `RoomRegistryActor::ListRooms`,
  asks each `RoomActor` for its config + occupant count + affiliation
  histogram. Reports occupant + per-tier affiliation counts. The
  `space_jid` filter parses but is a no-op in V2; future PRs link
  channels to spaces via the PubSub bookmark surface.
- `urn:waddle:admin:channels:create:0` — args `name` (1–80) + optional
  `topic` + optional `is_public` (default true). Mints a managed room
  localpart, creates the room with `persistent=true`,
  `members_only=!is_public`.
- `urn:waddle:admin:channels:update:0` — patches name/topic/is_public
  via the actor's `UpdateConfig`.
- `urn:waddle:admin:channels:delete:0` — `confirm='yes'` required;
  delegates to `RoomRegistryActor::DestroyRoom`.
- `urn:waddle:admin:channels:occupants:0` — `RoomActor::ListOccupants`
  paged; surfaces nick/real_jid/role/affiliation.
- `urn:waddle:admin:channels:affiliations:0` — `RoomActor::ListAffiliations
  { filter }` paged; filter accepts owner/admin/member/none/outcast.
- `urn:waddle:admin:channels:set-affiliation:0` —
  `RoomActor::ChangeAffiliation`; outcast = XEP-0045 §10.2 ban.
- `urn:waddle:admin:channels:kick:0` — looks up the occupant by
  bare-JID-to-nick mapping and asks the actor to `Leave`. The
  XEP-0045 §9.1 §307 presence broadcast happens via the wire-side
  machinery in #680; this admin path is best-effort state mutation when
  the occupant is currently joined.

**Why Waddle-namespaced?** Same justification as spaces: XEP-0133 lacks
the cursor-paginated CRUD shape. The underlying MUC operations conform
to XEP-0045 §10 (affiliations) and §9.1 (kick role-change to none).

**Tests**: `server/crates/waddle-server/tests/admin_channels_ws.rs` —
20 #[tokio::test] cases. Unit tests under
`server/crates/waddle-server/src/admin/channels.rs::tests` cover the
wire-affiliation vocabulary, the public-by-default default, and result-form
shape.

## Audit log

(empty — first audit starts at XEP-0004 unless a higher-impact target is chosen first)
