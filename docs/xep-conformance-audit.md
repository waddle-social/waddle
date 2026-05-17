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
| 0012 | Last Activity                                        | jabber:iq:last                     |  Y  |  Y   |   Y   | unaudited  | |
| 0030 | Service Discovery                                    | http://jabber.org/protocol/disco#* |  Y  |  Y   |   Y   | unaudited  | disco/info.rs |
| 0045 | Multi-User Chat                                      | http://jabber.org/protocol/muc     |  Y  |  Y   |   Y   | unaudited  | muc_* feature family |
| 0047 | In-Band Bytestreams                                  | http://jabber.org/protocol/ibb     |  -  |  Y   |   -   | unaudited  | Impl exists, no advert? |
| 0048 | Bookmarks (legacy)                                   | storage:bookmarks                  |  -  |  Y   |   -   | unaudited  | Superseded by 0402 |
| 0049 | Private XML Storage                                  | jabber:iq:private                  |  Y  |  Y   |   ?   | unaudited  | Shared test xep0054_0049_0191 |
| 0050 | Ad-Hoc Commands                                      | http://jabber.org/protocol/commands|  Y  |  Y   |   -   | unaudited  | |
| 0054 | vcard-temp                                           | vcard-temp                         |  Y  |  Y   |   Y   | unaudited  | |
| 0059 | Result Set Management                                | http://jabber.org/protocol/rsm     |  -  |  Y   |   -   | unaudited  | Helper for MAM etc. |
| 0060 | Publish-Subscribe                                    | http://jabber.org/protocol/pubsub  |  Y  |  Y   |   Y   | unaudited  | Many sub-features advertised |
| 0065 | SOCKS5 Bytestreams                                   | http://jabber.org/protocol/bytestreams |  Y |  ?   |   -   | unaudited  | socks5_bytestreams in features |
| 0077 | In-Band Registration                                 | jabber:iq:register                 |  -  |  Y   |   -   | unaudited  | |
| 0080 | User Location                                        | http://jabber.org/protocol/geoloc  |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0082 | XMPP Date and Time Profiles                          | (profiles only)                    |  -  |  Y   |   -   | unaudited  | Profile usage, no advert |
| 0084 | User Avatar                                          | urn:xmpp:avatar:metadata           |  Y  |  Y   |   Y   | unaudited  | PEP node + notify |
| 0085 | Chat State Notifications                             | http://jabber.org/protocol/chatstates |  Y |  Y   |   Y   | unaudited  | |
| 0092 | Software Version                                     | jabber:iq:version                  |  Y  |  Y   |   Y   | unaudited  | |
| 0106 | JID Escaping                                         | (no namespace, escape rules)       |  -  |  Y   |   -   | unaudited  | |
| 0107 | User Mood                                            | http://jabber.org/protocol/mood    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0108 | User Activity                                        | http://jabber.org/protocol/activity |  - |  Y   |   Y   | unaudited  | PEP node |
| 0115 | Entity Capabilities                                  | http://jabber.org/protocol/caps    |  Y  |  Y   |   Y   | unaudited  | |
| 0118 | User Tune                                            | http://jabber.org/protocol/tune    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0153 | vCard-Based Avatars                                  | vcard-temp:x:update                |  -  |  Y   |   -   | unaudited  | |
| 0160 | Best Practices for Handling Offline Messages         | msgoffline                         |  Y  |  ?   |   Y   | unaudited  | |
| 0163 | Personal Eventing Protocol                           | http://jabber.org/protocol/pubsub#pep |  Y |  Y   |   Y   | unaudited  | |
| 0172 | User Nickname                                        | http://jabber.org/protocol/nick    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0184 | Message Delivery Receipts                            | urn:xmpp:receipts                  |  Y  |  Y   |   Y   | unaudited  | |
| 0191 | Blocking Command                                     | urn:xmpp:blocking                  |  Y  |  Y   |   Y   | unaudited  | |
| 0198 | Stream Management                                    | urn:xmpp:sm:3                      |  Y  |  Y   |   Y   | unaudited  | |
| 0199 | XMPP Ping                                            | urn:xmpp:ping                      |  Y  |  Y   |   Y   | unaudited  | |
| 0201 | Best Practices for Message Threads                   | (none, &lt;thread/&gt; element)            |  -  |  Y   |   Y   | gap        | Advertised `urn:xmpp:threads:0` (Waddle-fabricated). **PR #609** drops the advert; XEP-0201 wire behavior preserved + regression guard added. |
| 0202 | Entity Time                                          | urn:xmpp:time                      |  Y  |  Y   |   Y   | unaudited  | |
| 0203 | Delayed Delivery                                     | urn:xmpp:delay                     |  -  |  Y   |   Y   | unaudited  | |
| 0223 | Persistent Storage of Public Data via PubSub         | (profile of PEP)                   |  -  |  Y   |   -   | unaudited  | |
| 0237 | Roster Versioning                                    | urn:xmpp:features:rosterver        |  Y  |  Y   |   Y   | unaudited  | |
| 0249 | Direct MUC Invitations                               | jabber:x:conference                |  -  |  Y   |   -   | unaudited  | |
| 0277 | Microblogging over XMPP                              | urn:xmpp:microblog:0               |  -  |  ?   |   Y   | unaudited  | Test exists, impl path unclear |
| 0280 | Message Carbons                                      | urn:xmpp:carbons:2 + rules:0       |  Y  |  Y   |   Y   | unaudited  | |
| 0292 | vCard4 over XMPP                                     | urn:xmpp:vcard4 + +notify          |  Y  |  Y   |   -   | unaudited  | |
| 0297 | Stanza Forwarding                                    | urn:xmpp:forward:0                 |  -  |  Y   |   -   | unaudited  | Used by carbons / MAM |
| 0300 | Use of Cryptographic Hash Functions in XMPP          | urn:xmpp:hashes:2                  |  -  |  Y   |   -   | unaudited  | |
| 0308 | Last Message Correction                              | urn:xmpp:message-correct:0         |  Y  |  Y   |   Y   | unaudited  | |
| 0313 | Message Archive Management                           | urn:xmpp:mam:2 + :2#extended       |  Y  |  Y   |   Y   | unaudited  | Plus Waddle fulltext / thread extensions |
| 0317 | Hats                                                 | urn:xmpp:hats:0                    |  Y  |  Y   |   Y   | gap (partial) | Server-side authority/hat conflation fixed in **PR #611**. Dedicated test suite added (`tests/xep0317_hats.rs`). Outstanding: client-side `roleHatsForOccupant` mirror (B), MessageCard chip rewiring (C), Waddle-namespace hat URIs (D). |
| 0319 | Last User Interaction in Presence                    | urn:xmpp:idle:1                    |  -  |  Y   |   -   | unaudited  | |
| 0333 | Chat Markers                                         | urn:xmpp:chat-markers:0            |  Y  |  Y   |   Y   | unaudited  | |
| 0334 | Message Processing Hints                             | urn:xmpp:hints                     |  -  |  Y   |   Y   | unaudited  | |
| 0352 | Client State Indication                              | urn:xmpp:csi:0                     |  Y  |  ?   |   -   | unaudited  | Feature::csi exists, impl path? |
| 0357 | Push Notifications                                   | urn:xmpp:push:0                    |  Y  |  Y   |   Y   | unaudited  | |
| 0359 | Unique and Stable Stanza IDs                         | urn:xmpp:sid:0                     |  Y  |  Y   |   Y   | unaudited  | |
| 0363 | HTTP File Upload                                     | urn:xmpp:http:upload:0             |  Y  |  Y   |   -   | unaudited  | |
| 0372 | References                                           | urn:xmpp:reference:0               |  Y  |  Y   |   Y   | unaudited  | |
| 0377 | Spam Reporting                                       | urn:xmpp:reporting:1               |  -  |  Y   |   -   | unaudited  | |
| 0392 | Consistent Color Generation                          | (no namespace, algorithm)          |  -  |  Y   |   -   | unaudited  | |
| 0393 | Message Styling                                      | urn:xmpp:styling:0                 |  -  |  Y   |   -   | unaudited  | |
| 0401 | Easy User Onboarding                                 | urn:xmpp:invite                    |  -  |  Y   |   -   | unaudited  | |
| 0402 | PEP Native Bookmarks                                 | urn:xmpp:bookmarks:1               |  Y  |  Y   |   -   | unaudited  | bookmarks2 + compat |
| 0410 | MUC Self-Ping (Schrödinger's Chat)                   | (uses ping, optimization advert)   |  Y  |  Y   |   -   | unaudited  | |
| 0421 | Anonymous unique occupant identifiers for MUCs       | urn:xmpp:occupant-id:0             |  Y  |  Y   |   Y   | unaudited  | |
| 0424 | Message Retraction                                   | urn:xmpp:message-retract:1         |  Y  |  Y   |   Y   | unaudited  | |
| 0425 | Message Moderation                                   | urn:xmpp:message-moderate:1        |  Y  |  Y   |   Y   | unaudited  | |
| 0428 | Fallback Indication                                  | urn:xmpp:fallback:0                |  Y  |  Y   |   -   | unaudited  | |
| 0430 | Inbox                                                | urn:xmpp:inbox:0                   |  Y  |  Y   |   -   | unaudited  | Advert added at server crate level |
| 0431 | -                                                    | (assigned)                         |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0433 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0437 | Room Activity Indicators                             | urn:xmpp:rai:0                     |  -  |  Y   |   -   | unaudited  | |
| 0444 | Message Reactions                                    | urn:xmpp:reactions:0               |  Y  |  Y   |   Y   | unaudited  | |
| 0445 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0446 | File Metadata Element                                | urn:xmpp:file:metadata:0           |  -  |  Y   |   -   | unaudited  | |
| 0447 | Stateless File Sharing                               | urn:xmpp:sfs:0                     |  -  |  Y   |   -   | unaudited  | |
| 0448 | Encryption for stateless file sharing                | urn:xmpp:esfs:2                    |  -  |  Y   |   -   | unaudited  | |
| 0449 | Stickers                                             | urn:xmpp:stickers:0                |  -  |  Y   |   -   | unaudited  | |
| 0452 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0461 | Message Replies                                      | urn:xmpp:reply:0                   |  Y  |  Y   |   Y   | unaudited  | |
| 0469 | Bookmark Pinning                                     | urn:xmpp:bookmarks-pinning:0       |  -  |  Y   |   -   | unaudited  | |
| 0470 | Pubsub Attachments                                   | urn:xmpp:pubsub-attachments:1      |  -  |  Y   |   -   | unaudited  | |
| 0471 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0472 | Pubsub Social Feed                                   | urn:xmpp:pubsub-social-feed:0      |  -  |  Y   |   -   | unaudited  | |
| 0486 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0488 | Pinning Chat Messages                                | urn:xmpp:pin:0                     |  -  |  Y   |   -   | unaudited  | |
| 0492 | Chat Notification Settings                           | urn:xmpp:notification-settings:0   |  -  |  Y   |   -   | unaudited  | |
| 0500 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0501 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0502 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0503 | Spaces                                               | urn:xmpp:spaces:0                  |  Y  |  Y   |   Y   | unaudited  | Advert gated until owner subs done |
| 0508 | -                                                    |                                    |  -  |  Y   |   -   | unaudited  | Verify XEP number |
| 0513 | Explicit Mentions                                    | urn:xmpp:mentions:0                |  Y  |  Y   |   Y   | unaudited  | Plus `#channel` profile |

## Currently identified gaps (preliminary, pre-deep-audit)

### 1. `urn:xmpp:threads:0` advertised without spec backing

`Feature::threads()` in `server/crates/waddle-xmpp-core/src/disco/info/features.rs:86` advertises the namespace `urn:xmpp:threads:0`. A scan of `./xeps/xep-*.xml` and `./xeps/inbox/*.xml` finds no XEP or ProtoXEP that defines this namespace. XEP-0201 (Best Practices for Message Threads) uses the bare `<thread/>` element in messages and does not define a disco feature.

This violates the CLAUDE.md hard rule: "Do not use official XEP namespaces for Waddle-specific semantics."

Resolution options:
1. Drop the advertisement (matches XEP-0201 reality — no feature needed).
2. Move to `urn:waddle:threads:0` if the advert encodes Waddle-specific thread semantics that callers depend on.

Decision pending: check what consumers (clients, tests) read this advert before choosing.

## Audit log

(empty — first audit starts at XEP-0004 unless a higher-impact target is chosen first)
