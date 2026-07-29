# TODO — LiveKit lanes

Prioritized execution lanes for all open LiveKit issues (2026-07-27). Full breakdown with rationale: [#1489 comment](https://github.com/waddle-social/waddle/issues/1489#issuecomment-5090247048). Lanes at the same priority run in parallel; `→` marks a "Blocked by" dependency, `∥` marks independent work.

## P0 — Lane 0: production correctness & security

Live defects, no roadmap dependency — do these first.

- [✅] #1445 cross-replica call registry split — ~29% of joins denied (`room_not_found`)
- [ ] #1594 re-assert MUC media grants cross-node on non-owning-replica webhook (follow-up to merged #1593)
- [ ] #1444 callee LiveKit token leaked to caller via undeliverable session-initiate IQ error
- [ ] #1446 hang-up can leave camera/mic/transport live (hot mic)
- [ ] #1449 durable, generation-aware call control plane (webhook idempotency, SID-guarded teardown, revocation, Muji DoS) — consider splitting
- [ ] #1450 client call lifecycle hardening (engine stranding, localStorage tokens, TURN refresh)
- [ ] #1488 `waddle.call.setup.ok` counts undeliverable 1:1 invites as success

## P1 — Wave 0 foundation (five parallel lanes)

### Lane 1: version currency + TURN (epics #1506/#1507/#1508)

- [ ] #1530 bump livekit-client to ^2.21.0 ∥ #1529 TURN config prerequisites + chart drift fix
- [ ] #1447 resolve TURN authority (XEP-0215 coturn vs embedded LiveKit TURN) — before the v1.13.x jump
- [ ] #1531 soak livekit-server v1.12.0 + verify TURN relay → #1532 upgrade to v1.13.4
- [ ] #1533 Renovate on LiveKit artifacts + version-currency policy (independent)

### Lane 2: SFU HA + reconnect (epics #1509/#1510/#1512)

- [ ] #1534 Redis Sentinel → #1535 SFU reads room state from Redis → #1536 three replicas with sysload node selection
- [ ] #1451 residual: tuned probes, image digest pinning, strict config (alongside #1536)
- [ ] #1537 surge-then-drain rollout safety → #1539 autoscaling with floor of three
- [ ] #1538 reconcile stale room-node mappings (after #1536)
- [ ] #1543 fast-path reconnect on node loss → #1544 reconnecting/rejoined call UI
- [ ] #1554 automatic secret-rotation propagation (after #1537 — rotation before rollout safety = automated outages)

### Lane 3: host controls (epics #1513/#1517) — unblocked now via merged #1593

- [ ] #1558 mute-all ∥ #1559 remove-from-call ∥ #1560 lock-call
- [ ] #1585 viewer-only participant role (whenever convenient; also feeds Lane 12)

### Lane 4: multi-tenancy + security hardening (epics #1514/#1515/#1516)

- [ ] #1547 tenant-keyed credential lookup → #1548 mint from tenant keys ∥ #1549 webhook verify from issuer → #1550 remove credential singletons → #1551 multi-key SFU chart
- [ ] #1552 per-tenant admission controls (after #1548; per-tenant keys must land before real multi-tenant traffic)
- [ ] Day-1 independents: #1556 webhook rate limit · #1555 NodePort perimeter · #1553 TURN-realm docs · #1557 encryption-posture docs

### Lane 5: call SLOs (epic #1511; gated on Lane 1's #1532)

- [ ] #1452 residual first: ICE/TURN instrumentation, webhook observability, call correlation ID, per-participant QoS — the plumbing the SLOs alert on
- [ ] #1540 join success/latency SLOs → #1541 drop-rate/node-availability SLOs → #1542 media-quality SLOs

## P2 — Wave 1 credibility

### Lane 6: recording (epic #1519; absorbs #1023/#1031/#1033)

- [ ] #1564 deploy LiveKit Egress (needs Lane 2's #1534) → #1565 start/stop with participant notice (+ XEP-0050 surface from #1031) → #1566 tenant-prefixed storage (needs #1552) → #1567 membership-scoped retrieval → #1568 enforced retention
- [ ] #1033 residual: call-thread artifact (after #1567)

### Lane 7: waiting room (epic #1518) — unblocked now

- [ ] #1561 admission protocol → #1562 admit/deny UI ∥ #1563 waiting participant experience

### Lane 8: audit logging (epic #1520) — unblocked now

- [ ] #1569 call lifecycle audit events → #1570 occupant-id subjects + admin attribution

## P3 — Wave 2 reach

### Lane 9: scheduled meetings + guest join (epics #1521/#1522)

- [ ] #1571 room from calendar event → #1572 join link → #1573 recurring meetings
- [ ] #1574 invite token model → #1575 OAUTHBEARER guest auth → #1576 guest session restrictions → #1577 guest badge/audit (needs #1570) ∥ #1578 gate through waiting room (needs #1562)

### Lane 10: captions / STT (epic #1524)

- [ ] #1582 deploy self-hosted STT → #1583 transcribe live audio → #1584 live captions

### Lane 11: breakouts (epic #1523)

- [ ] #1579 create/assign breakout rooms (needs #1536) → #1580 recall to parent call → #1581 breakout moderation UI

## P4 — Wave 3 enterprise

### Lane 12: webinars (epic #1525)

- [ ] #1585 → #1586 broadcast tier design (design around the room-fits-one-node ceiling) → #1587 capacity verify → #1588 webinar host controls (needs #1560)

### Lane 13: compliance suite (epic #1526)

- [ ] #1589 discovery search (needs #1568 + #1570) → #1590 legal hold ∥ #1591 DLP hooks
- [ ] #1592 multilingual searchable transcripts (needs #1584 + #1568)

## Backlog — adjacent, slot opportunistically

- [ ] #971 zero-blip deploys PRD (partially overtaken by #1537)
- [ ] #1017 Zoom-style call window layout parity
- [ ] #945 DM↔MUC parity (1:1 call completion)
- [ ] #1166 lazy-load LiveKit (~600KB) off first paint
- [ ] #1162 infra CI hardening (helps every infra lane land safely)
