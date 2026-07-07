# TODO — Open Issue Order

Audit of all open issues against main @ `794057be` (2026-07-07). Every issue was
verified against actual code, not just issue state. Ordering: Tier 0 first;
within a tier, lanes run in parallel and bullets within a lane run top-to-bottom.
Bundles marked `[one PR]` touch the same files and should land together.

## Tier 0 — Close / rescope on GitHub (no implementation)

| Issue | Action |
|---|---|
| #466 | Close — XEP-0359 §3 `by`-verification live (`chat/src/lib/xmpp/wasm-message-codecs.ts:607`) |
| #395 | Close — DM identity disambiguated (avatars + presence dot in `DmPanel.vue`) |
| #389 | Close — composer placeholder tracks active conversation |
| #386 | Close — channel settings separated from destructive delete (ConfirmDialog wired) |
| #1147 | Close — duplicate, fully superseded by epic #1195 |
| #92 | Rescope or close — offline invites now durable (XEP-0160 pending-delivery); residual = reject/retract call-thread projection |
| #472 | Rescope — HATS conflation fixed + test-pinned; residual = XEP-0050 hat CRUD + durable hat store |
| #231 | Park — only non-anonymous rooms exist; occupant-id authz branch not needed until semi-anon ships |
| #317 | Verify then likely close — presence/DND epic (ADR-010) shipped; residual is custom status = #366 |
| #945 | Keep open until SFU lane B done (residue = #1127/#1128/#1130/#1131), then close |
| #1017 | Keep open until recording chain (#1023 → #1031 → #1033) lands, then close |

## Tier 1 — Criticals (start immediately)

1. **#1159** — CNPG backups → R2 for both Postgres clusters (**no backups exist today**). HITL: secrets + restore drill.
2. **#1161** — supply chain: drop `packages:write` from PR pipelines, sign + verify Flux artifacts. Partly HITL (GHCR perms).

## Tier 2 — XEP/RFC conformance violations + top reliability (parallel lanes)

### Lane A — MUC conformance (server)
1. #1111 — RoomFull join → `service-unavailable` presence error (XEP-0045 §7.2.9); currently empty reply. S.
2. #1172 — MUC MUST gaps: status 201 on create, config broadcast 104/170/171, PM routing, `<decline/>`, affiliation-list authz (§9 admin-only). M–L.
3. #1169 — ISR: implement XEP-0397 wire shape or drop `urn:xmpp:isr:0` advert; replace `format!`-XML + substring parsing. M.
4. #1150 — fix all 7 wire deviations (0452, 0500, 0502, 0446, 0433, 0488, 0448). L.

### Lane B — SFU / calls reliability `[one PR bundle]`
1. #1131 — survivor hangup gets `<forbidden/>`; XEP-0166 §6.7 wants item-not-found/unknown-session or idempotent ack. Pairs with #1128.
2. #1128 — webhook cleanup no-op for 1:1 calls (BareJid parse gate).
3. #1130 — forwarded call IQ to offline peer silently dropped → synthesize `service-unavailable` (RFC 6120 §8.5.3).
4. #1127 — reconciler mass-teardown on LiveKit restart.
5. #1129 — `clear_local_state` clobbers concurrent joiner.
6. #1142 — one JWT per `<content/>` burns JTI budget. S.
7. #1140 — backdate join-token `nbf` for clock skew. S.

### Lane C — Push server reliability + conformance
1. #1123 + #1124 + #1126 `[one PR]` — duplicate/spurious push trio: partial-success retry re-sends delivered devices; janitor sweeps live transient flush claims; suppress at unread=0 + retry jitter.
2. #779 + #780 `[one PR]` — preserve XEP-0359 stanza-id through XEP-0357 publish (SW dedupe is dead by construction); suppress push for XEP-0444 reaction-only messages.
3. #774 + #772 + #773 + #776 + #777 `[one PR]` — XEP-0357/0050 polish: strict publish-options FORM_TYPE validation, emit SessionExpired, session-lifecycle robustness, single-tx register-device, typed composer errors.
4. #775 — finish §6 forward cleanup on 410 GONE (device half done; user-server invalidation + client notify remain).
5. #1125 — dead-letter permanently-unresolvable notification candidates.

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
6. #1124 — (listed in Lane C bundle; lives in pending_delivery).
7. #1188 — deflake xep0115 caps test (ping FIFO anchor).

### Lane I — Infra / CI (after Tier 1 items)
1. #1162 — PR-time infra CI + chart diff-fail publish + HelmRelease remediation + ESO redundancy.
2. #1160 — required status checks + merge_group + gate container publish on XMPP compliance. HITL (rulesets).
3. #1163 — alerts-as-code + Alloy pod-log collection. Contact-point routing HITL.

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

Lanes **A–I run concurrently** (disjoint files). Within Tier 2 that is up to
9 simultaneous work streams. Only hard cross-lane dependencies:

- #1087 → #1102
- #1096 → #1088 → #1195-Ph3 / #971 (Tier 5 chain)
- #1023 (HITL) → #1033
- #752 → #753; #435 → #436 / retires #445
- #1120 co-designed with #1037 item 4
- HITL items needing the owner: #1159, #1160, #1023, #870, #1092, #1148, #1088/#1195/#971 direction
