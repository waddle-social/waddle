# TODO — Open Issue Order

Audit of all open issues against main @ `794057be` (2026-07-07). Every issue was
verified against actual code, not just issue state. Ordering: Tier 0 first;
within a tier, lanes run in parallel and bullets within a lane run top-to-bottom.
Bundles marked `[one PR]` touch the same files and should land together.

**Update 2026-07-10:** a second deep audit of 1:1 + group chat vs main @ `c35afb9d`
added **Lane J** (#1243–#1268, epic #1269) — the highest-impact chat/MUC defects.

## Tier 0 — Close / rescope on GitHub (no implementation)

| Issue | Action |
|---|---|
| #466 | Close — XEP-0359 §3 `by`-verification live for DM identity (`chat/src/lib/xmpp/wasm-message-codecs.ts`, PR #863). Residual (unverified-`by` catch-up dedupe in `rawMessageSeenIds`) tracked in #1267 |
| #395 | Close — DM identity disambiguated (avatars + presence dot in `DmPanel.vue`) |
| #389 | Close — composer placeholder tracks active conversation |
| #386 | Close — channel settings separated from destructive delete (ConfirmDialog wired) |
| #1147 | Close — duplicate, fully superseded by epic #1195 |
| #92 | Rescope or close — offline invites now durable (XEP-0160 pending-delivery); residual = reject/retract call-thread projection |
| #472 | Rescope — HATS conflation fixed + test-pinned; residual = XEP-0050 hat CRUD + durable hat store |
| #231 | Park — only non-anonymous rooms exist; occupant-id authz branch not needed until semi-anon ships |
| #317 | Verify then likely close — presence/DND epic (ADR-010) shipped; residual is custom status = #366 |
| #945 | Keep open until the four issues that gate it (#1127/#1128/#1130/#1131, each "Relates to #945") are done, then close. Lane B's other items (#1129/#1140/#1142) are general SFU hygiene, not #945-gating |
| #1017 | Keep open until recording chain (#1023 → #1031 → #1033) lands, then close |

## Tier 1 — Criticals (start immediately)

1. **#1159** — CNPG backups → R2 for both Postgres clusters (**no backups exist today**). HITL: secrets + restore drill.
2. **#1161** — supply chain: drop `packages:write` from PR pipelines, sign + verify Flux artifacts. Partly HITL (GHCR perms).

## Tier 2 — XEP/RFC conformance violations + top reliability (parallel lanes)

### Lane A — MUC conformance (server)
1. ✅ **#1111 — RoomFull join → `service-unavailable` presence error (XEP-0045 §7.2.9); was an empty reply. DONE (PR #1212, merged).**
2. ✅ **#1172 — MUC MUST gaps: status-201 on create, config broadcast (104/170/171), §7.5 PM routing (+ groupchat-to-occupant `bad-request`, normal-typed PM, anti-spoof), §7.8 mediated decline (oracle-closed), affiliation-list authz (§9 Owner/Admin), admin/owner overflow-join (§7.2.9). Anonymity/`whois` scope creep left to #231. DONE (PR #1214, merged).**
3. ✅ **#1169 — ISR: dropped `urn:xmpp:isr:0` advert + bespoke `format!`-XML token scheme (removal was the conformant choice; nothing depended on it). DONE (PR #1211, merged).**
4. ✅ **#1150 — fixed all 7 wire deviations (0452 `<mentions>`, 0500 field name, 0502 disco field / removed invented subscribe, 0446 `<length>` ms, 0433 XEP-0004 form + RSM, 0488 token text-content, 0448 `<encrypted/>` in `<sources>`). DONE (PR #1216, merged).**

**Lane A COMPLETE.** (Lane B complete.)

### Lane B — SFU / calls reliability `[one PR bundle]` — ✅ **DONE (PR #1210, merged)**
1. ✅ #1131 — survivor hangup no longer `<forbidden/>`; XEP-0166 §6.7 4-case handler (ack / item-not-found + unknown-session / forbidden). Also: an undeliverable session-terminate now acks (hangup succeeds).
2. ✅ #1128 — 1:1 webhook cleanup keyed on raw CallId; MUC/BareJid Muji path preserved.
3. ✅ #1130 — offline-peer request IQ → typed `service-unavailable` (echoes payload, RFC 6120 §8.3.1); transient failures not misreported.
4. ✅ #1127 — reconciler two-pass absence streak before teardown.
5. ✅ #1129 — `clear_local_state` atomic `emptied` gate, no concurrent-joiner clobber.
6. ✅ #1142 — one JWT per participant shared across contents.
7. ✅ #1140 — join-token `nbf` backdated by shared 30s skew constant.

_Residual (tracked, not #945-gating): survivor-check TOCTOU needs per-call locking → #1148/#1195._

### Lane C — Push server reliability + conformance
1. ✅ **#1123 + #1124 + #1126 `[one PR]` — duplicate/spurious push trio: per-device idempotent retry (delivered-device filter + all-delivered→published), janitor claim-recency floor (`claimed_at_ms` + 3-interval floor), unread-0 suppression (typed `Suppressed` outcome + `unread_zero_at_publish` audit reason) + ±25% retry jitter on all three backoff paths. DONE (PR #1236).**
2. ✅ **#779 + #780 `[one PR]` — stanza-id rides a typed `stanza-id` attribute on `urn:waddle:push:context:0` → `PushEnvelope.item` (item id stays job id for coalesce/idempotency; SW dedupe now live for DM + MUC); reaction-only messages suppressed at T1 with typed `xep0444_reaction` reason (archived, never pushed). DONE (PR #1239).**
3. ✅ **#774 + #772 + #773 + #776 + #777 `[one PR]` — XEP-0357/0050 polish: tri-state publish-options FORM_TYPE (`bad-request` on mismatch), typed `<session-expired/>` vs `<bad-sessionid/>` (bounded recently-expired cache), minted sessionid on single-shot Completed, single-tx register-device (+XEP-0060 compensation), typed `PushRegistrationError` + `StanzaError.application_condition` + structured WASM rejection + chat session-expired retry. DONE (PR #1240).**
4. ✅ **#775 — §6 forward cleanup on 410 GONE: `disable_registration_tx` transitions the (jid,node) registration to `disabled` in the worker's finalize tx when the last active device is permanently gone (one 410 with a live sibling keeps it enabled per §6.1); `get_for_user` stops producing targets so no further candidates enqueue. Client notify stays under #762. DONE (PR #1241).**
5. ✅ **#1125 — dead-letter permanently-unresolvable candidates: `MAX_CANDIDATE_POLICY_ATTEMPTS = 48` (~4h at the saturated 5-min backoff), then terminal suppression with typed `policy_retries_exhausted` reason + `outboxed_at_ms` so existing retention pruning reaps the row; fresh candidates no longer starved. DONE (PR #1242).**

### Lane D — MAM conformance + read path
1. #1173 — to-less query targets sender's own archive (RFC 6120 §10.3 / XEP-0313 §4.1); currently bad-request. S.
2. #1171 — XEP-0428 offsets: Unicode code points, not UTF-16 units (server + client + tests). S.
3. #1112 + #1115 + #1116 `[one PR]` — read-path: lazy COUNT(*), cap `ids` filter + index-or-withdraw fulltext, max=0 count-probe must not claim complete.
4. #1113 + #1095 `[one PR]` — `(room_jid, stanza_id)` index + indexed origin-id correction lookup (XEP-0308 fails past 100 rows today).
5. #1170 — mam:2#extended: implement flip-page + metadata or de-advertise (both are MUSTs of the advertised feature).
6. #1114 — retention task: actually call `delete_before` (zero production callers).

### Lane E — PubSub / PEP
1. #1118 — emit XEP-0060 §7.2.2.1 retract + §8.4.1 delete notifications (advertised `notify_retract/delete=true`, never fire). M.
2. #1121 — bound the XEP-0115 caps pending-resolution map (TTL + per-resource cap). DoS hygiene. S–M.
3. #1119 — pubsub subscribe: unique index + transactional upsert. S.
4. #1117 — PEP fan-out: kill N+1 blocking-list queries, defer fan-out off the publish path. After #1087 helps.
5. #448 + #453 + #449 `[one PR]` — avatar SET-path idempotence probe (shared with backfill probe) + typed HashValue.
6. #1120 — feed/stories retention: **design together with #1037 item 4** — XEP-0472 base profile MUSTs `max_items=max`, so read-clamp + MAM steering, not eviction.
7. #1037 — remaining social-feed hardening: rate limit, `pubsub#meta-data` disco form, base-profile SHOULDs.
8. #208 — umbrella: closes once #1118 + #240 land.

### Lane F — Chat client (protocol-facing)
1. #444 — **highest-leverage PEP item**: advertise `urn:xmpp:avatar:metadata+notify` / `urn:xmpp:vcard4+notify` caps + dispatch avatar/vcard4 PEP events. The entire server-side avatar push is inert without it. M.
2. #677 — Cmd+R scroll doesn't anchor to newest message (8-frame cap insufficient). M.
3. #1167 — shared modal a11y primitive (focus trap/restore) + combobox ARIA + timeline live region. M.
4. #445 — queue workspace member JIDs for avatar pull (retired if #435 lands first). S.

### Lane G — Hygiene quick wins (agent-ready, all S)
1. #1144 — `env::remove_var` after threads exist (glibc UB).
2. #1174 + #1136 `[coordinate]` — delete dead code (parser.rs, room_registry.rs, format!-vCard) + wire-or-remove dead metrics.
3. #1133 — drop unbounded per-room metric label.
4. #1175 — log hygiene (demote expected disconnects, rate-limit eviction warn, scrub JIDs).
5. #1176 — docs/ADR rot (mark superseded GraphQL-era ADRs, purge README/CLAUDE.md).
6. #984 — nodeprep/PRECIS in `username_to_localpart` (RFC 6122). M, needs migration.
7. #302 — zeroize config-layer secrets.

### Lane H — SM / session persistence residuals
1. #1197 + #1198 `[one PR]` — poison SM rows: skip-and-log on decode + quarantine/purge at rest.
2. #1138 + #1100 `[one PR]` — stop re-stamping `detached_at` on snapshot + incremental detached-queue append (same code path).
3. #1206 — residual: durable codec drops presence payloads on restart (`persistence_codec.rs:158`); in-memory path fixed by #1207.
4. #1137 — residual: wrap-aware `record_detached_outbound_at` + sort (2^32 boundary only).
5. #1132 — residual: state-inventory collector still raw asks without timeout (janitor half fixed by #1207).
6. #1188 — deflake xep0115 caps test (ping FIFO anchor).

### Lane I — Infra / CI (after Tier 1 items)
1. #1162 — PR-time infra CI + chart diff-fail publish + HelmRelease remediation + ESO redundancy.
2. #1160 — required status checks + merge_group + gate container publish on XMPP compliance. HITL (rulesets).
3. #1163 — alerts-as-code + Alloy pod-log collection. Contact-point routing HITL.

### Lane J — 1:1 chat + MUC conformance (2026-07-10 audit; epic #1269) — ✅ **COMPLETE**

**Lane J COMPLETE (2026-07-11):** all six original bundles merged — J1 client chat
(PR #1273, #1275 server stanza-id strip), J2 server 1:1 routing (PR #1272), J3 MUC invites/
lifecycle (PR #1276), J4 MUC-MAM/spoofing (PR #1274), J5 MUC delivery/self-ping
(PR #1277), J6 disco truthfulness (PR #1271). 27 issues closed (#1243–#1268 + #1275);
epic **#1269** closed. Each implementation bundle merged after CI green, a successful
final-SHA Greptile check, the Qodo report's actionable findings were addressed, and
zero unresolved review threads remained. Post-completion residuals are tracked below.

Second deep audit (6 lanes incl. independent gpt-5.5) of private + group chat vs
main @ `c35afb9d`; all findings verified in code. #1243–#1268 filed, indexed by
epic **#1269**. Both features are broadly implemented — the user-visible breakage
is the High/Critical items below. Sub-bundles touch mostly-disjoint files and run
in parallel; within a bundle, top-to-bottom.

**J1 — Client chat `[one PR bundle]`**
1. ✅ **#1243 — High: WASM core now unwraps normal XEP-0280 carbons (§11 own-bare-JID verified, forged envelopes fully ignored) and surfaces the inner message with a `carbon` direction marker; TS renders/dedupes it (dead `carbon:*` compat handlers deleted). DONE (PR #1273).**
2. ✅ **#1256 — MUC PMs (`type=chat` from a known room's occupant JID) file under the full occupant JID; replies/chat-states/reactions/corrections address `room@service/nick` with full-occupant-JID sender checks. DONE (PR #1273).**
3. ✅ **#1258 — caps now advertise message-retract:1, reactions:0, message-correct:0, sid:0 (implement ⇒ advertise; ver-string recomputed). moderate:1 deliberately omitted — XEP-0425 defines it for the groupchat service only. DONE (PR #1273).**
4. ✅ **#1255 — `join_room` always sends `<history maxstanzas='0'/>` (XEP-0045 §7.2.15); MAM catch-up is authoritative; `join_room_without_history` variant deleted. DONE (PR #1273).**
5. ✅ **#1267 — client 1:1 minor cluster: 0424 fallback marker, `by`-verified catch-up dedupe (#466 residual), first-stanza-id consumers fixed, resumed catch-up budget-gap affordance, correction guards (no tombstone resurrection, occupant-JID match); `Date.now` item = forwarded-`<delay>` propagation (residual merge-window documented in PR #1273). DONE (PR #1273).**

**J2 — Server 1:1 routing / MAM / receipts `[bundle]`**
1. ✅ #1244 — **Critical**: full-JID chat to an offline resource silently dropped (no bare-JID fallback / offline store / error; RFC 6121 §8.5.3.2.1). DONE (PR #1272)
2. ✅ #1245 — full-JID DM to a detached SM resource bypasses recipient MAM/stanza-id/carbons (#1106 fixed bare-JID only). DONE (PR #1272)
3. ✅ #1246 — message to nonexistent local user persisted instead of `<service-unavailable/>` (RFC 6121 §8.5.1). DONE (PR #1272)
4. ✅ #1247 — server fabricates XEP-0184 receipts (dup/false receipts; presence oracle). DONE (PR #1272)
5. ✅ #1266 — 1:1 minor cluster (8 items). Coordinate with Lane D #1173 (to-less MAM) — same read path. DONE (PR #1272)

**J3 — MUC invites / lifecycle `[bundle]`**
1. ✅ #1248 — **High**: mediated invitations (§7.8) unimplemented for non-group-DM rooms → "invite" silently no-ops. Relates #945. DONE (PR #1276)
2. ✅ #1252 — **High**: nick-change attempts no longer tear down MUJI/SFU state; Waddle's identity-locked nickname policy rejects them conformantly with `not-acceptable` rather than emitting a 303 rename. DONE (PR #1276)
3. ✅ #1261 — destroy doesn't wipe durable state (resurrection) + misses sibling same-nick sessions. DONE (PR #1276)
4. ✅ #1264 — mediated decline spoofable (no invite ledger) + dropped for offline/remote inviter. DONE (PR #1276)
5. ✅ #1262 — owner bypasses §8.4/§9.7 role-change target protections. DONE (PR #1276)

**J4 — MUC-MAM + occupant-id + spoofing `[bundle]`**
1. ✅ #1250 — **High**: MUC-MAM result envelopes missing `from` → strict clients discard all room history. DONE (PR #1274)
2. ✅ #1251 — **High (security)**: client `muc#user <x>` not stripped from groupchat messages before reflect/archive (persisted spoofing). DONE (PR #1274)
3. ✅ #1268 — XEP-0421 occupant-id gaps (PM, destroy presence, MAM real-JID for non-anon rooms). DONE (PR #1274)

**J5 — MUC delivery / self-ping reliability `[bundle]`**
1. ✅ #1249 — **High**: cross-node disconnect cleanup ghosts occupants — typed `MucProxyRouteDecision` splits benign local-room from harmful origin-claim-elsewhere, remote-resource origin fixes the second-device case, reconciliation janitor re-drives failed relays. DONE (PR #1277)
2. ✅ #1253 — self-ping now matches all `occupant_sessions` for the nick (multi-session rejoin loop fixed). DONE (PR #1277)
3. ✅ #1254 — reaped/dormant room self-ping answers `<not-acceptable/>` (XEP-0410 not-joined) so clients rejoin. DONE (PR #1277)
4. ✅ #1257 — MUC PMs route through the SM/detached delivery envelope + XEP-0313 archives + XEP-0280 sent-carbons + `muc#roomconfig_allowpm`. DONE (PR #1277)
5. ✅ #1263 — `<item-not-found/>` bounce for nonexistent rooms, bounded `DroppedFull` retries + drop counter on reflection/presence fan-out. DONE (PR #1277)

**J6 — Disco truthfulness**
1. ✅ #1259 — duplicate feature + duplicate `muc#roominfo` FORM_TYPE make disco#info ill-formed (XEP-0115 §5.4). DONE (PR #1271)
2. ✅ #1260 — disco#info on a nonexistent room fabricates an open room (should be `item-not-found`). DONE (PR #1271)
3. ✅ #1265 — XEP-0045 MUC minor conformance cluster (16 items, incl. `muc#stable_id` advertise, history-knob, member-list access). Relates Lane D #1170 (extended-MAM), #1258. DONE (PR #1271; items 3 → #232 and 5 deferred with justification in the PR)

_Cross-refs (already-lane'd, referenced not re-filed): #1173/#1171/#1170 (Lane D), #1118 (Lane E), #984 (Lane G), #466 (Tier 0 close — residual → #1267)._

### Lane J follow-up — adversarial residual audit (2026-07-11; epic #1279) — 🚧 **IN PROGRESS**

The original Lane J scope above remains historical completion. A subsequent adversarial
audit found residual loss, isolation, concurrency, and conformance defects. Work the
remaining unblocked frontier.

1. ✅ **#1280 — XEP-0198 handler-timeout false ack: cancelled message/presence dispatch remains sender-owned, `h` cannot cross the hole, and cleanup detaches without hanging. DONE (PR #1292, merged).**
2. ✅ **#1281 — scope MUC-PM MAM history to the full occupant JID. DONE (PR #1293, merged).**
3. ✅ **#1287 — scope MUC-PM displayed state to the full occupant JID. DONE (PR #1308, merged).**
4. ✅ **#1282 — reject untrusted delay timestamps on ordinary MUC messages while preserving conformant foreign-authority stanza IDs. DONE (already implemented by PR #324, merged: authenticated ingress strips it in `server/crates/waddle-server/src/server/routes/websocket/handlers/message.rs`; `xep0203_delayed_delivery_ws::client_spoofed_delay_is_stripped_from_groupchat_flow` injects a forged occupant delay and proves it absent from live reflection and MAM replay).**
5. ✅ **#1288 — prevent cross-occupant MUC message-ID collision merges. DONE (PR #1304, merged).**
6. #1289 — make mediated-invite grants and rollback atomic.
7. #1283 — seal room destruction and purge the destroyed MAM epoch.
8. ✅ **#1290 — implement XEP-0313 `with=self` intersection semantics. DONE (PR #1309, merged).**
9. ✅ **#1286 — advertise XEP-0085 chat-state support in entity capabilities. DONE (PR #1307, merged).**
10. ✅ **#1291 — reject duplicate `FORM_TYPE` values across capability forms. DONE (PR #1310, merged).**
11. ✅ **#1284 — reconcile `RoomActor` claims left by dead nodes. DONE (PR #1335, merged; exact-ownership prerequisites PRs #1332–#1334 and restart follow-up PR #1340).**
12. #1285 — send MUC status 332 on non-resumable service shutdown.

_Shared completion work: #1136 proves the handler-timeout metric end to end; its completion
unblocks #1163's timeout alert. #1174 removes the post-#1247 dead XEP-0184 surface and
stale receipt comments._

## Tier 3 — Performance / scale prep (parallel with Tier 2 tail)

1. #1087 — bare-JID connection index (blocks #1102).
2. #1102 — presence broadcast batching (after #1087).
3. #1109 — per-session joined-rooms index (O(rooms) disconnect scan).
4. #1096 — de-serialize global DB access (do **before** #1088).
5. #237 — Postgres-backed store tests in CI (de-risks #1088).
6. #153 — refinery migrations (with/before #1088).
7. #1166 — lazy-load LiveKit (~600KB) + emoji data off first paint.

## Tier 4 — Features (each independent)

1. #366 — custom-status PEP node (confirmed never built; reuse status-preference infra). M–L.
2. #961 + #752 — DM pinned conversations (XEP-0469 module exists, unwired; #950 closed → unblocked) + sidebar from XEP-0402 bookmarks. Related plumbing.
3. #753 — client XEP-0115 caps cache (after #752 shrinks the fan-out).
4. #529 — APNs dispatch (unblocked now) → #530 FCM → #531 observability/e2e → close epic #506.
5. #435 — workspace co-membership → RFC 6121 roster (unblocks #363 fan-out, retires #445) → #436 conformant user deletion.
6. #363 — avatar/vCard PEP sync epic: server done; closes via #444 (+ #437 if lifting the 100KB cap).
7. #1031 — recording control via XEP-0050 against Egress trait (unblocked, implementable against a fake now).
8. #213 — XEP coverage manifest + CI advertisement↔test gate (**pull forward if possible** — structurally prevents #1169/#1150-class drift) → then #214 ejabberd-parity triage.
9. #232 — XEP-0045 §7.2.16 history-on-join (parsed but never replayed; SHOULD-level).
10. #370 — RSS integration MVP (XMPP-native server-authored messages; docs only so far). L.
11. #368 — personal saved views (`urn:waddle:views:0` justified — no covering XEP). L.
12. #437 — binary-payload object storage for pubsub items. L.
13. #240 — PEP Presence/Roster access models in `can_subscribe` (currently owner-only fail-closed).
14. #210 — normalize IM archive semantics: features all exist; remaining work = unified cross-surface test matrix. Mostly audit.
15. #212 — PEP integration coverage gaps (0398, retract/delete tests — blocked on #1118).
16. #235 — typed builders in integration tests (16 files still `format!`).
17. #251 — finish typing client `ArchivedMessage`.
18. #398, #396, #394, #392, #383, #497, #475, #477 — frontend small fry: title unread count, hide dev metadata, roster empty state, retract confirm, update banner, ThreadPanel prop, reaction-on-retracted guard, typed inbound events (L, makes #475 trivial).
19. #822 children — link previews: #869 (composer pre-send card, S) → #868 (server og:video verify, M) → #870 (allowlist — ship/no-ship decision, HITL) → #833 (cleanup sweeper, blocked on #828/#829) → #831 (E2EE gating, hard-blocked on E2EE mode).
20. #717 — Apple app parity backlog (multi-PR, HITL device/signing).

## Tier 5 — Architecture / HITL-gated (sequential chain, needs owner input)

```
#1096 → (#237, #153) → #1088 Postgres → #1195 Phase 3–4 ⇄ #971 zero-blip → #1092 ecdysis decision → #1148 registry sharding
```

- #1195 — ADR-0017 scaling epic (Phases 0–2 done; Phase 3 = cross-node routing, XEP-0198 resume across nodes).
- #971 — zero-downtime PRD; **reconcile with #1195 Phase 3 into one roadmap before starting**.
- #1088 — global DB → Postgres (blocks Phase 3 and #971).
- #1092 — implement or delete dead ecdysis restart path (design decision).
- #1148 — shard RoomRegistryActor (design decision, direction set by Phase 3).
- #282 — persistent room state machine (sentinel; may be triggered by Phase 3 durable rooms).
- #284, #300 — routing decommission + legacy MUC presence builders (fold into the above refactors).
- #1023 — deploy livekit-egress + secret (HITL, fragile 1Password/ESO path; PR #1069 open) → unblocks #1033 → close #1017.
- #215 — server parity epic (tracker over #208/#210/#212/#213/#214).
- #1195/#1147/#506/#822/#945/#1017 — epics: close as their children complete.

## Max parallelism summary

Lanes **A–J run concurrently** (disjoint files). Within Tier 2 that is up to
10 simultaneous work streams (Lane J's six bundles J1–J6 are themselves parallel).
Only hard cross-lane dependencies:

- #1087 → #1102
- #1096 → #1088 → #1195-Ph3 / #971 (Tier 5 chain)
- #1023 (HITL) → #1033
- #752 → #753; #435 → #436 / retires #445
- #1120 co-designed with #1037 item 4
- Lane J soft cross-refs: J2 ⇄ Lane D #1173 (shared MAM read path); J6 ⇄ Lane D #1170; J5 #1249 ⇄ #1195 (clustering); J1 #1255 ⇄ #232
- HITL items needing the owner: #1159, #1160, #1023, #870, #1092, #1148, #1088/#1195/#971 direction
