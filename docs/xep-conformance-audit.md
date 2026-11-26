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
| 0004 | Data Forms                                           | jabber:x:data                      |   -   |   Y    |    Y    | fixed      | Building block, used by many. Multi-item results (`<reported/>` + `<item/>`) carry no top-level fields; the paging cursor moves to an XEP-0059 `<set/>`. Space `pubsub#node_config` submissions parse through the typed form, ignore unknown fields and treat omitted settings as unchanged. Waddle form fields are Clark-namespaced (#2745). Tests: `waddle-xmpp/tests/xep0004_data_forms.rs`. |
| 0012 | Last Activity                                        | jabber:iq:last                     |   Y   |   Y    |    Y    | fixed      | Last-activity queries require presence authorization (#2670). Last logout is persisted (#2708). |
| 0030 | Service Discovery                                    | disco#info, disco#items (http://jabber.org/protocol/disco#*) |   Y   |   Y    |    Y    | fixed      | disco/info.rs. Extension bots (`<plugin>@extensions.<domain>`, bare or `/bot`) answer as `client/bot` with no items; other extensions-domain addresses return item-not-found. A room's extension bots are listed under disco#items node `urn:waddle:room:bots:0` (empty for users who can't read the room). Account disco#info requires presence authorization and returns `service-unavailable` for missing or unauthorized accounts (#2667). Unknown server disco#info nodes return `item-not-found` (#2672). disco#info and disco#items responses mirror the requested node, and unknown JID+node pairs on accounts, upload, calls, community and push return `item-not-found` from the queried address; the service-list fallback applies only to the server domain (#2707). Evidence: `xep0030_account_disco_privacy_ws.rs`, `xep0030_unknown_node_ws.rs`. |
| 0045 | Multi-User Chat                                      | http://jabber.org/protocol/muc     |   Y   |   Y    |    Y    | partial    | muc_* feature family. New rooms are locked until the owner configures them (#2712). Voice requests are relayed to moderators and affiliation lists carry `nick` (#2762). IQs in unsupported namespaces get `service-unavailable` (#2691). The Waddle room config field no longer squats the `muc#roomconfig_` prefix; Waddle form fields are Clark-namespaced (#2745). Open: voice requests for a cluster-remote room node and S2S moderator approvals. |
| 0047 | In-Band Bytestreams                                  | http://jabber.org/protocol/ibb     |   -   | helpers only |    Y    | not-implemented | Not advertised by design: IBB is client-to-client and the server has no role. `Feature::ibb()` and the `xep0047` helpers have no production caller. Tests: `waddle-xmpp/tests/xep0047_in_band_bytestreams.rs`. |
| 0049 | Private XML Storage                                  | jabber:iq:private                  |   Y   |   Y    |    Y    | fixed      | Dedicated suite `waddle-xmpp/tests/xep0049_private_xml_storage.rs`, plus the shared `xep0054_0049_0191_ws.rs`. The legacy `storage:bookmarks` bridge was removed with XEP-0048 (#2690): `storage:bookmarks` is ordinary opaque private storage. Child-namespace and missing/invalid-child handling follows the XEP (#2749). |
| 0050 | Ad-Hoc Commands                                      | http://jabber.org/protocol/commands |   Y   |   Y    |    Y    | fixed      | `execute` on an existing session, absent `action`, requester `status`, errors that leave a session correctable, `cancel` forms and `xml:lang` on responses are handled per XEP-0050 (#2742). Tests: `waddle-server/tests/xep0050_ad_hoc_commands_ws.rs`, `waddle-xmpp/tests/xep0050_ad_hoc_commands.rs`. |
| 0054 | vcard-temp                                           | vcard-temp                         |   Y   |   Y    |    Y    | fixed      | A vCard get for another user with no vCard returns `service-unavailable` (#2749). |
| 0055 | Jabber Search                                        | jabber:iq:search                   |   Y   |   Y    |    Y    | conformant | Nick-only field set. Tests: `waddle-server/tests/xep0055_user_search_ws.rs`. |
| 0059 | Result Set Management                                | http://jabber.org/protocol/rsm     |   -   |   Y    |    Y    | fixed      | Helper for MAM etc. MAM `<first/>` carries `index` and RSM `<index/>` is honoured (#2710). Tests: `waddle-xmpp/tests/xep0059_result_set_management.rs`. |
| 0060 | Publish-Subscribe                                    | http://jabber.org/protocol/pubsub  |   Y   |   Y    |    Y    | fixed      | Many sub-features advertised. `pubsub#publish-options` is enforced and advertised (#2693). Delete and purge notify subscribers, and parse, subscribe and instant-node errors use the XEP-0060 conditions (#2740). Roster-model nodes follow the roster access model with roster groups (#2703). Open: space-node `notify_sub` and `notify_config` (see 0503). |
| 0065 | SOCKS5 Bytestreams                                   | http://jabber.org/protocol/bytestreams |   -   |   -    |    -    | not-implemented | Not advertised and not implemented: `Feature::socks5_bytestreams()` has no production caller and a test asserts it is absent. File transfer uses XEP-0363 and XEP-0447. |
| 0077 | In-Band Registration                                 | jabber:iq:register                 |   -   | library only |    Y    | not-implemented | `xep0077.rs` has no served path; IQs in unsupported namespaces, `jabber:iq:register` included, get `service-unavailable` (#2691). Tests: `waddle-xmpp/tests/xep0077_in_band_registration.rs`. |
| 0080 | User Location                                        | http://jabber.org/protocol/geoloc  |   -   |   -    |    Y    | unaudited  | PEP node. Generic PEP node classification only (`PepHandler::is_well_known_node`); there is no dedicated module and no client support. |
| 0082 | XMPP Date and Time Profiles                          | (profiles only)                    |  -  |  Y   |   -   | unaudited  | Profile usage, no advert |
| 0084 | User Avatar                                          | urn:xmpp:avatar:metadata           |  Y  |  Y   |   Y   | unaudited  | PEP node + notify; avatar nodes default `open` (not XEP-0163 `presence`) so non-roster peers resolve avatars; legacy `presence` avatar nodes without an owner-configure marker are reopened at startup/publish (pre-marker explicit `presence` choices are indistinguishable and are reset) |
| 0085 | Chat State Notifications                             | http://jabber.org/protocol/chatstates |  Y |  Y   |   Y   | fixed      | Web (#2676) and Apple (#2677) clients now have a setting to disable chat-state notifications (§ MUST). |
| 0092 | Software Version                                     | jabber:iq:version                  |  Y  |  Y   |   Y   | unaudited  | |
| 0106 | JID Escaping                                         | (no namespace, escape rules)       |   -   |   Y    |    Y    | fixed      | `escape_node` follows the spec's escaping rules (#2715). Tests: `waddle-xmpp/tests/xep0106_jid_escaping.rs`. |
| 0107 | User Mood                                            | http://jabber.org/protocol/mood    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0108 | User Activity                                        | http://jabber.org/protocol/activity |  - |  Y   |   Y   | unaudited  | PEP node |
| 0115 | Entity Capabilities                                  | http://jabber.org/protocol/caps    |   Y   |   Y    |    Y    | fixed      | Disco#info identity `xml:lang` is read from the XML namespace so the verification hash matches (#2666). Hash handling for unsupported hashes, fields without a value and ill-formed checks follows the XEP (#2768). |
| 0118 | User Tune                                            | http://jabber.org/protocol/tune    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0128 | Service Discovery Extensions                         | jabber:x:data in disco#info        |   -   |   Y    |    Y    | unaudited  | Extension forms in disco#info and their XEP-0115 hash input. Evidence: `xep0115_caps_ws.rs`, `xep0357_push_service_ws.rs`. |
| 0153 | vCard-Based Avatars                                  | vcard-temp:x:update                |   -   | partial (helpers only) |    Y    | unaudited  | Helpers have no production caller. Tests: `waddle-xmpp/tests/xep0153_vcard_avatars.rs`. |
| 0156 | Discovering Alternative XMPP Connection Methods      | host-meta                          |   -   |   Y    | inline  | unaudited  | `waddle-server/src/server/routes/well_known.rs`; tests are inline (`xep0156_host_meta_tests`). |
| 0160 | Best Practices for Handling Offline Messages         | msgoffline                         |   Y   |   Y    |    Y    | fixed      | Offline messages stored while the priority is negative are flushed when the same session returns to a non-negative priority (#2708). Tests: `xep0160_priority_reflush_ws.rs`, `xep0160_ordered_pending_ws.rs`. |
| 0163 | Personal Eventing Protocol                           | identity `pubsub/pep` (no feature var) |   Y   |   Y    |    Y    | fixed      | The `pubsub#pep` feature var is not advertised; XEP-0163 defines none, and support is signalled by the `pubsub/pep` identity plus the XEP-0060 features of `pep_features()` (#2749). Last published item is sent to newly-available resources on presence (#2668) and to explicit subscribers (#2749). Contacts can read presence- and roster-model nodes they are authorized for (#2673); the roster access model honours roster groups (#2703). Notifications reach all of the owner's connected resources with `+notify`, including the publishing resource (#2684). PEP subscriptions end when the presence subscription is lost (#2749). Tests: `waddle-server/tests/xep0163_pep_ws.rs`. |
| 0166 | Jingle                                               | urn:xmpp:jingle:1                  |   Y   |   Y    |    Y    | gap        | The server relays 1:1 sessions and is the responder for Muji via `calls.<domain>`. Open: the Muji disposition check, and the mixer rejects follow-up actions that lack `<muji/>`. Tests: `waddle-xmpp/tests/xep0166_jingle/`, `calls_e2e_ws.rs`. |
| 0167 | Jingle RTP Sessions                                  | urn:xmpp:jingle:apps:rtp:1         |   Y   |   Y    |    Y    | partial    | Server `calls.<domain>` mixer advertises RTP features. Rust client (used by Apple/Android) advertises `urn:xmpp:jingle:1`, `urn:xmpp:jingle:apps:rtp:1` and the audio/video media types (#2681); no ICE-UDP/DTLS features, since media uses the LiveKit transport. 1:1 session-initiate/accept descriptions carry payload types (#2709). Open: the Muji mixer rewrites payload types on rebroadcast. Tests: `waddle-xmpp/tests/xep0167_jingle_rtp.rs`. |
| 0172 | User Nickname                                        | http://jabber.org/protocol/nick    |  -  |  Y   |   Y   | unaudited  | PEP node |
| 0178 | Best Practices for Use of SASL EXTERNAL with Certificates |                                    |   -   |   Y    |    Y    | conformant | S2S only. Tests: `waddle-xmpp/tests/xep0178_s2s_negotiation.rs`. |
| 0184 | Message Delivery Receipts                            | urn:xmpp:receipts                  |   -   |   -    |    Y    | conformant | Not advertised: the server routes `<request/>` and `<received/>` verbatim and does not generate acks (#1247). Tests: `waddle-xmpp/tests/xep0184_message_delivery_receipts.rs`. |
| 0185 | Dialback Key Generation and Validation               |                                    |   -   |   Y    |    Y    | conformant | S2S only. Tests: `waddle-server/tests/xep0220_s2s_dialback/keys.rs`. |
| 0191 | Blocking Command                                     | urn:xmpp:blocking                  |   Y   |   Y    |    Y    | fixed      | IQ get/set to or from a blocked local bare JID is gated in both directions (#2702). Block items may carry XEP-0377 `<report/>` (#2694). The shared Rust client and UniFFI bridge support block-list retrieval, block, targeted unblock, unblock-all, and trusted pushes (#2462). Client tests cover wire shapes, driver acknowledgment, and FFI argument validation. Client suite: `waddle-xmpp-client/src/blocking/tests.rs`, `waddle-xmpp-client-ffi/src/blocking_tests.rs`. |
| 0198 | Stream Management                                    | urn:xmpp:sm:3                      |   Y   |   Y    |    Y    | fixed      | Malformed `<resume/>` or ack and a second `<enable/>` are rejected (#2706). Credential lifecycle IQs (`urn:waddle:credential:1`) use idempotent replay nonces and opaque results/errors (`payload: None` on error) so `sm_unacked` never stores secret material. |
| 0199 | XMPP Ping                                            | urn:xmpp:ping                      |  Y  |  Y   |   Y   | unaudited  | |
| 0201 | Best Practices for Message Threads                   | (none, &lt;thread/&gt; element)    |   -   |   Y    |    Y    | conformant | `<thread/>` and its `parent` attribute are preserved. No `urn:xmpp:threads:0` is advertised; the Waddle threads query uses `urn:waddle:threads:0`. Tests: `waddle-xmpp/tests/xep0201_thread_parent.rs`. |
| 0202 | Entity Time                                          | urn:xmpp:time                      |  Y  |  Y   |   Y   | unaudited  | |
| 0203 | Delayed Delivery                                     | urn:xmpp:delay                     |  -  |  Y   |   Y   | unaudited  | |
| 0215 | External Service Discovery                           | urn:xmpp:extdisco:2                |   Y   |   Y    |    Y    | gap        | `services` get returns stun + turns entries. Open: `type='turn'` filter matches no entry and `<credentials/>` is unsupported. Tests: `waddle-xmpp/tests/xep0215_external_service_discovery.rs`. |
| 0220 | Server Dialback                                      | jabber:server:dialback / urn:xmpp:features:dialback |   -   |   Y    |    Y    | fixed      | Admission-gated (ADR-0020 D2), not offered by default. Dialback errors and multiplexed pairs are handled without terminating the stream (#2717). Tests: `waddle-server/tests/xep0220_s2s_dialback.rs`. |
| 0223 | Persistent Storage of Private Data via PubSub        | (profile of PEP)                   |   -   | partial |    -    | unaudited  | Profile of PEP. `publish-options` is enforced and advertised on PEP publish (#2693). Open: clients do not check publish-options support before storing private data. |
| 0237 | Roster Versioning                                    | urn:xmpp:features:rosterver        |   Y   |   Y    |    Y    | conformant | XEP Obsolete; behaviour is now RFC 6121 §2.6. Tests: `xep0237_roster_versioning_ws.rs`. |
| 0249 | Direct MUC Invitations                               | jabber:x:conference                |   -   |   Y    |    Y    | conformant | Wire helpers only: direct invitations are client-to-client and the server relays them as ordinary messages. Tests: `waddle-xmpp/tests/xep0249_direct_muc_invitations.rs`. |
| 0272 | Multiparty Jingle (Muji)                             | urn:xmpp:jingle:muji:0             |   Y   |   Y    |    Y    | gap        | The SFU mixer at `calls.<domain>` is the responder. Open: payload types are rewritten on rebroadcast, and follow-up actions without `<muji/>` are rejected. Tests: `waddle-xmpp/tests/xep0272_muji.rs`. |
| 0277 | Microblogging over XMPP                              | urn:xmpp:microblog:0               |   -   |   -    |    Y    | unaudited  | Test exists. Node classification only (`PepHandler`); not implemented. Open: the node uses PEP default `max_items` and last-item delivery. |
| 0280 | Message Carbons                                      | urn:xmpp:carbons:2 + rules:0       |   Y   |   Y    |    Y    | partial    | `urn:xmpp:carbons:rules:0` advertised. XEP-0249 and mediated invitations are carbon-copied, and error replies to carbon-eligible messages are carbon-copied on the local pipeline (#2716). Open: carbon copies of error bounces from room, ingress and unserved paths, and received carbons for ingress-rejected messages. |
| 0292 | vCard4 over XMPP                                     | urn:ietf:params:xml:ns:vcard-4.0 (PEP node `urn:xmpp:vcard4`) + +notify |   Y   |   Y    |    Y    | fixed      | Canonical `access_model=open` per Security Considerations, wired through `NodeConfig::pep_for_node(PEP_NODE_VCARD4)` + reconcile-on-publish for legacy `presence`-access nodes in **PR #669**. Dedicated suite: `crates/waddle-xmpp/tests/xep0292_vcard4.rs` + `crates/waddle-server/tests/xep0292_vcard4_ws.rs`. `urn:xmpp:contacts` node defaults to `whitelist` (#2679). Open: TEL parsing ignores the `<uri>` value. |
| 0297 | Stanza Forwarding                                    | urn:xmpp:forward:0                 |   -   |   Y    |    Y    | unaudited  | Used by carbons / MAM. Tests: `waddle-xmpp/tests/xep0297_stanza_forwarding.rs`. |
| 0298 | Conference Information in Jingle (Coin)              | urn:xmpp:coin:1                    |   -   | partial |    -    | gap        | The mixer sets the `isfocus` marker on a Muji session-accept only; `urn:xmpp:coin:1` is not advertised. |
| 0300 | Use of Cryptographic Hash Functions in XMPP          | urn:xmpp:hashes:2                  |   -   |   Y    |    Y    | unaudited  | Used by XEP-0448 and server profile publishing; the Rust client advertises `urn:xmpp:hashes:2` and SHA-256 (#2692). Tests: `waddle-xmpp/tests/xep0300_hashes.rs`. |
| 0308 | Last Message Correction                              | urn:xmpp:message-correct:0         |  Y  |  Y   |   Y   | unaudited  | |
| 0313 | Message Archive Management                           | urn:xmpp:mam:2 + :2#extended       |   Y   |   Y    |    Y    | partial    | Plus Waddle fulltext / thread extensions. Archive metadata, `<flip-page/>`, RSM index and `forbidden` for a foreign archive implemented (#2710). The sender's archive stanza-id is withheld from 1:1 recipients (#2726). Open: the Rust client does not verify the result `from` against the queried archive. |
| 0317 | Hats                                                 | urn:xmpp:hats:0                    |  Y  |  Y   |   Y   | partial    | Authority stays in XEP-0045 (#611). The deployment assigns hats through `ServerHats`; every server-built occupant presence (join, late-joiner replay, role/affiliation change, leave) carries them, and client-sent hats are stripped. Today only extension bots wear one (`urn:waddle:hats:bot`). Hat list/assign commands (§3.2–3.6) are not implemented. Tests: `waddle-xmpp/tests/xep0317_hats.rs`. |
| 0319 | Last User Interaction in Presence                    | urn:xmpp:idle:1                    |   -   |   Y    |    Y    | unaudited  | Tests: `waddle-xmpp/tests/xep0319_last_user_interaction.rs`. |
| 0333 | Displayed Markers                                    | urn:xmpp:chat-markers:0            |   Y   |   Y    |    Y    | fixed      | Markers are read through the marked message only: a stale marker, or one for a message not in the chat, does not clear newer unread (#2741). |
| 0334 | Message Processing Hints                             | urn:xmpp:hints                     |  -  |  Y   |   Y   | unaudited  | Hints are bypassed on bare/server JIDs, so credential lifecycle operations use `<iq/>` (`urn:waddle:credential:1`) rather than `<message/>`. |
| 0352 | Client State Indication                              | urn:xmpp:csi:0                     |   -   |   -    |    -    | not-implemented | Neither advertised nor implemented. `Feature::csi()` has no production caller, and Bind 2 does not offer CSI inline. |
| 0357 | Push Notifications                                   | urn:xmpp:push:0                    |   Y   |   Y    |    Y    | fixed      | Third-party (client app) push services are published to over S2S (#2728). |
| 0359 | Unique and Stable Stanza IDs                         | urn:xmpp:sid:0                     |  Y  |  Y   |   Y   | unaudited  | |
| 0363 | HTTP File Upload                                     | urn:xmpp:http:upload:0             |   Y   |   Y    |    Y    | fixed      | Slot `<put/>` headers are limited to Authorization, Cookie and Expires; `Content-Type` is no longer sent (#2680). Uploads are served as sandboxed attachments and long non-ASCII filenames truncate safely (#2704). Slots, upload service disco#info and PUT validation follow XEP-0363 (#2730). Tests: `waddle-xmpp/tests/xep0363_http_upload.rs`, `xep0363_upload_slot_ws.rs`. |
| 0368 | SRV records for XMPP over TLS                        | _xmpps-server._tcp                 |   -   |   Y    |    Y    | conformant | S2S, outbound discovery. Tests: `waddle-server/tests/xep0368_s2s_discovery.rs`. |
| 0372 | References                                           | urn:xmpp:reference:0               |   Y   |   Y    |    Y    | unaudited  | The Rust client advertises `urn:xmpp:reference:0` (#2692). Broadcast mentions are no longer sent as references; `@everyone`/`@here` use XEP-0513 `#channel`. |
| 0377 | Blocking Command Reports                             | urn:xmpp:reporting:1               |   -   |   Y    | Y (helpers only) | fixed      | Block items may carry `<report/>` (#2694). Tests: `waddle-xmpp/tests/xep0377_spam_reporting.rs`, `waddle-server/tests/xep0377_block_report_ws.rs`. |
| 0386 | Bind 2                                               | urn:xmpp:bind:0                    |   -   |   Y    |    Y    | fixed      | Implemented (#2753) but reachable only where SASL2 negotiation is enabled, which is a test-support capability: production does not offer SASL2. Tests: `waddle-server/tests/xep0386_bind2_ws.rs`, `waddle-xmpp-core/tests/xep0386_bind2.rs`. |
| 0388 | Extensible SASL Profile                              | urn:xmpp:sasl:2                    |   -   |   Y    |    Y    | not-implemented | Test-support only: SASL2 is not offered in production. Tests: `waddle-xmpp-client/tests/xep0388_sasl2.rs`, `waddle-xmpp-core/tests/xep0388_sasl2_authenticate.rs`. |
| 0392 | Consistent Color Generation                          | (no namespace, algorithm)          |   -   |   Y    | inline  | fixed      | HSLuv RGB generation; CVD corrections and the DJB2 fallback are removed; the web client has a colourization toggle (#2766). Tests are inline unit tests in `waddle-xmpp-core/src/xep0392.rs`. Open: toggle on Android, Apple and Linux. |
| 0393 | Message Styling                                      | urn:xmpp:styling:0                 |   -   | Y (library only) |    Y    | fixed      | No production caller. Preformatted blocks keep leading blank lines (#2765). Tests: `waddle-xmpp/tests/xep0393_message_styling.rs`. |
| 0394 | Message Markup                                       | urn:xmpp:markup:0                  |   -   | client |    Y    | fixed      | Built and parsed in `waddle-xmpp-client`. Child elements match by namespace, and same-range inline styles merge into one span (#2765). Tests: `waddle-xmpp-client/tests/xep0394_message_markup.rs`. |
| 0398 | User Avatar to vCard-Based Avatars Conversion        |                                    |   -   |   -    |    -    | not-implemented | Not advertised; cited in code comments only. |
| 0401 | Ad-hoc Account Invitation Generation                 | urn:xmpp:invite#invite             |   -   | data model only, not wired |    Y    | unaudited  |  |
| 0402 | PEP Native Bookmarks                                 | urn:xmpp:bookmarks:1               |   -   |   Y    |    Y    | fixed      | The PEP node `urn:xmpp:bookmarks:1`. No disco feature is advertised: `#compat-pep` (#2675) and `#compat` (#2690) were dropped with the legacy bridge, and `Feature::bookmarks2()` has no production caller. Publishes enforce `publish-options` (#2693). Tests: `waddle-xmpp/tests/xep0402_pep_native_bookmarks.rs`. |
| 0410 | MUC Self-Ping (Schrödinger's Chat)                   | http://jabber.org/protocol/muc#self-ping-optimization (room JIDs only) |   Y   |   Y    |    Y    | unaudited  |  |
| 0421 | Occupant identifiers for semi-anonymous MUCs         | urn:xmpp:occupant-id:0             |   Y   |   Y    |    Y    | partial    | Moderation and retraction tombstones in MAM keep the author's occupant-id, and the service advertises `urn:xmpp:occupant-id:0` (#2763). Open: occupant-id on mediated invite and decline relays. |
| 0422 | Message Fastening                                    | urn:xmpp:fasten:0                  |   -   |   -    |    Y    | partial    | Carrier only for `urn:waddle:safety-scores:1` and call-thread-ended. The Rust client advertises `urn:xmpp:fasten:0` (#2692). Open: `clear='true'` is ignored by the client. Tests: `waddle-xmpp-client/tests/xep0422_safety_scores.rs`. |
| 0424 | Message Retraction                                   | urn:xmpp:message-retract:1         |   Y   |   Y    |    Y    | fixed      | Retraction tombstones the message and its XEP-0308 corrections in MAM, keeping `<replace/>` and copying the retraction `<reason/>` (#2671). A retraction naming a correction id tombstones that correction and keeps its `<replace/>` (#2763). |
| 0425 | Moderated Message Retraction                         | urn:xmpp:message-moderate:1        |   Y   |   Y    |    Y    | fixed      | A tombstone of a retracted XEP-0308 correction keeps `<replace/>`, and a forged `<moderated/>` in an occupant-authored retraction is pruned (#2763). |
| 0428 | Fallback Indication                                  | urn:xmpp:fallback:0                |   Y   |   Y    |    Y    | gap        | Open: multiple offset-less body/subject children reject the whole indication, a childless `<fallback/>` is not treated as covering every body and subject, and mixed whole+range children are rejected. Tests: `waddle-xmpp/tests/xep0428_fallback_indication.rs`. |
| 0430 | Inbox                                                | urn:xmpp:inbox:1                   |   Y   |   Y    |    Y    | fixed      | Streaming `<message><entry/></message>` per conversation + closing `<iq type='result'><fin/></iq>` with `total`/`unread`/`all-unread`. Supports `unread-only` + `messages` filters, RSM `<max/>` paging, XEP-0333 displayed-marker → unread decrement. Mark-read remains a Waddle-private `urn:waddle:inbox:0` IQ-set until XEP-0333 covers 1:1 DMs end-to-end. ESL `erlang-solutions.com:xmpp:inbox:0` compat dropped (no active consumers). Tests: `server/crates/waddle-server/tests/xep0430_inbox_ws.rs`. PR #693. `<fin/>` `total`/`unread`/`all-unread` are inbox-wide, independent of filters and paging (#2678). Chat markers update unread for 1:1 chats as well as rooms (#2741). |
| 0431 | Full Text Search in MAM                              | urn:xmpp:fulltext:0                |   Y   |   Y    |    Y    | fixed      | Fulltext terms are escaped for `LIKE` and case-folded for Unicode in the sqlx MAM (#2752). Tests: `waddle-xmpp/tests/xep0431_fulltext_mam.rs`. |
| 0433 | Extended Channel Search                              | urn:xmpp:channel-search:0:search   |   Y   |   Y    |    Y    | fixed      | Search lists only public rooms (#2669). Empty or malformed requests return errors with the application condition, and the search form carries the required fields (#2761). Tests: `crates/waddle-server/tests/xep0433_channel_search_ws.rs`, `crates/waddle-xmpp/tests/xep0433_extended_channel_search.rs`. |
| 0437 | Room Activity Indicators                             | urn:xmpp:rai:0                     |   -   |   -    | helpers only | not-implemented | Not implemented on the wire; `xep0437.rs` is an internal unread tracker with no production caller. |
| 0444 | Message Reactions                                    | urn:xmpp:reactions:0               |  Y  |  Y   |   Y   | unaudited  | |
| 0445 | Pre-Authenticated In-Band Registration               | urn:xmpp:pars:0                    |   -   | helpers only, not wired |    Y    | not-implemented | Verify XEP number |
| 0446 | File Metadata Element                                | urn:xmpp:file:metadata:0           |   -   |   Y    |    Y    | unaudited  |  |
| 0447 | Stateless File Sharing                               | urn:xmpp:sfs:0                     |   -   |   Y    |    Y    | unaudited  |  |
| 0448 | Encryption for stateless file sharing                | urn:xmpp:esfs:0                    |   -   |   Y    |    Y    | fixed      | Encrypted shares require the plaintext `<size/>`: the client builder, FFI and WASM boundaries refuse to send one without it, and the parser ignores shares that lack it (#2682). |
| 0449 | Stickers                                             | urn:xmpp:stickers:0                |   -   |   Y    |    Y    | unaudited  |  |
| 0461 | Message Replies                                      | urn:xmpp:reply:0                   |   Y   |   Y    |    Y    | fixed      | Replies resolve by stanza-id or origin-id and the `to` attribute is optional (#2711). The Rust client advertises `urn:xmpp:reply:0` (#2692). |
| 0469 | Bookmark Pinning                                     | urn:xmpp:bookmarks-pinning:0       |   -   |   Y    |    Y    | unaudited  |  |
| 0470 | Pubsub Attachments                                   | urn:xmpp:pubsub-attachments:1      |   -   |   Y    |    Y    | unaudited  | Used for XEP-0501 story reactions; not advertised. Tests: `waddle-server/tests/xep0470_story_reactions_ws.rs`. |
| 0471 | Calendar Events                                      | urn:xmpp:calendar-events:0         |   -   |   -    |    -    | not-implemented | Placeholder. Waddle's calendar uses the xCal ProtoXEP: PEP node `urn:xmpp:calendar:0` with `urn:ietf:params:xml:ns:xcal` payloads. |
| 0472 | Pubsub Social Feed                                   | urn:xmpp:pubsub-social-feed:1      |   Y   |   Y    |    Y    | unaudited  | Tests: `waddle-server/tests/xep0472_social_feed_ws.rs`. |
| 0486 | MUC Avatars                                          | vcard-temp / muc#roominfo_avatarhash |   -   | helpers only | helpers only | not-implemented | Not implemented server-side. Rooms advertise `vcard-temp`, but a room vCard cannot be published. |
| 0488 | MUC Token Invite                                     | urn:xmpp:muc-token-invite:0        |   -   | helpers only |    Y    | not-implemented | Wire helpers have no production caller. |
| 0490 | Message Displayed Synchronization                    | urn:xmpp:mds:displayed:0           |   Y   |   Y    |    Y    | fixed      | PR #665. Well-known PEP node with whitelist + max-items=max + send-last=never defaults; chat publishes alongside XEP-0333 displayed, catches up + subscribes on connect. Client caps advertise `+notify`. The server enforces and advertises `pubsub#publish-options`, which gates MDS publishes (#2693). Server-assist (§3.5) is not implemented. |
| 0492 | Chat Notification Settings                           | urn:xmpp:notification-settings:1   |  -  |  Y   |   Y   | partial    | Wire+projection in `xep0492_chat_notification_settings`; gate enforced at DM push fan-out via `PushDispatchDecision::evaluate` — covered by `xep0492_push_enforcement_ws` (matrix) + `tests/messages.rs::xep0492_direct_chat_*` (wire). MUC push fan-out is not yet wired (`QueueOfflineDelivery` only fires for DMs); reducer matrix locks the contract for when it lands. |
| 0493 | OAuth Client Login                                   | OAUTHBEARER (RFC 7628)             |   -   |   Y    |    Y    | fixed      | OAuth discovery for an empty OAUTHBEARER request, the `xmpp:client:normal` scope and grant management (#2750). Tests: `waddle-server/tests/xep0493_oauth_ws.rs`. |
| 0494 | Client Access Management                             | urn:xmpp:cam:0                     |   -   |   Y    |    Y    | fixed      | Account owners list and revoke OAuth grants (#2750). Tests: `waddle-server/tests/xep0494_client_access_ws.rs`. |
| 0500 | MUC Slow Mode                                        | muc#roominfo_slow_mode_duration (no feature var) | roominfo field | helpers only |    Y    | partial    | The roominfo field is always emitted as 0 (disabled); there is no config or enforcement. |
| 0501 | Pubsub Stories                                       | urn:xmpp:pubsub-social-feed:stories:0 |   Y   |   Y    |    Y    | partial    | Open: the node access model is `open` rather than `presence`, there is no `pubsub#item_expire` or publisher retraction, and stories live on a shared community node rather than a per-user PEP node. Tests: `waddle-server/tests/xep0501_stories_ws.rs`. |
| 0502 | MUC Activity Indicator                               | {urn:xmpp:muc-activity}message-activity |   -   | local model only |    Y    | not-implemented | The disco field is not emitted. |
| 0503 | Server-side spaces                                   | urn:xmpp:spaces:0                  |   Y   |   Y    |    Y    | partial    | Space nodes are pinned to the XEP-0503 pubsub type and report `purge_offline` false (#2747). Open: `notify_sub` and `notify_config` are not sent, so they are not advertised. |
| 0508 | Forums                                               | urn:xmpp:forums:0                  |   -   |   -    |    -    | not-implemented | Waddle forums use `urn:waddle:forums:0`. |
| 0511 | Link Metadata                                        | rdf:Description (OGP) payload      |   -   |   Y    |    Y    | unaudited  | Tests: `waddle-xmpp/tests/xep0511_link_metadata.rs`, `waddle-server/tests/xep0511_s2s_account_link_metadata_ws.rs`. |
| 0513 | Intentional Mention Markup                           | urn:xmpp:mentions:0                |   Y   |   Y    |    Y    | fixed      | Plus `#channel` profile. Mentions are carried as `<mention type target active/>` inside XEP-0394 `<markup><span start end>`; Waddle-only `jid`/`uri`/`noping` moved to a sibling `urn:waddle:mentions:0` element (#2683). Client sends `@everyone` as `#channel` and `@here` as `#channel` with `active='true'`; the removed permissions IQ/form (`<query/>` in `urn:xmpp:mentions:0`) is no longer answered, and the count cap is local abuse-mitigation policy. |
| —    | (Waddle) Call lifecycle reports                      | urn:waddle:call-lifecycle:0        |  Y  |  Y   |   Y   | fixed      | Closed-set client operational reports; counted and acknowledged without call-state changes. Dedicated `xep_waddle_call_lifecycle` and `calls_e2e_ws` coverage. |
| —    | (Waddle) Inference credential lifecycle              | urn:waddle:credential:1            |  Y  |  Y   |   Y   | fixed      | Server-domain IQ-only client-sealed credential lifecycle (subkey fetch, device enrollment, credential enroll/status/rotate/revoke, policy mutation); stripped recursively across all IQ error-bounce paths. Dedicated core + server integration suites. |

## Currently identified gaps

None recorded here. Per-XEP gaps live in the `gap` and `partial` rows of the
master inventory. The earlier `urn:xmpp:threads:0` advertisement is gone:
`Feature::threads()` no longer exists, the server advertises only
`urn:waddle:threads:0` for the Waddle threads query, and XEP-0201 `<thread/>`
behaviour is covered by `waddle-xmpp/tests/xep0201_thread_parent.rs`.

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
- **Result** (`<x type='result'>`): reported columns
  `jid` / `display_name` / `has_owner_hat`; one `<item/>` per user. As
  XEP-0004 requires, the multi-item result has no top-level fields, so it
  carries no `FORM_TYPE`; when more results exist the `<command/>` carries
  an XEP-0059 `<set/>` whose `<last/>` is the cursor to send back as
  `after_cursor` (#2745).
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
as the `FORM_TYPE` for the args forms and single-entity result forms;
multi-item results carry none). Owner-gated via the same
`is_community_owner(state, requesting_jid)` chokepoint as Admin V1.

- `urn:waddle:admin:spaces:list:0` — paginated read; reports
  `space_jid`/`space_node`/`name`/`description`/`icon_url`/`channel_count`/
  `member_count`; the continuation cursor is an XEP-0059 `<set><last/></set>`
  on the `<command/>`, not a form field.
- `urn:waddle:admin:spaces:create:0` — args `name` (1–80) plus optional
  `description`/`icon_url`; mints a slugified space localpart, writes
  the metadata row + the backing PubSub node, seeds server-owner
  PubSub Owner affiliations.
- `urn:waddle:admin:spaces:update:0` — args `space_jid` + optional
  patch fields; rejects unknown space with `<item-not-found/>`.
- `urn:waddle:admin:spaces:delete:0` — args `space_jid` + `confirm` (must
  equal `"yes"`); cascade-destroys child MUC rooms via `DestroyRoom`,
  deletes the PubSub node and metadata row.
- `urn:waddle:admin:spaces:members:0` — paginated read (reported `jid` /
  `role`, no top-level fields) of PubSub
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

- 2026-11: reconciled against `main` after the merged audit fixes (#2666 to
  #2768). Open gaps are the `gap` and `partial` rows above.
