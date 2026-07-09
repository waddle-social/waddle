# Waddle switchable-alternative program

## Outcome

Waddle becomes a credible primary home for developer communities with 50–500
active members. Success means three pilot communities—at least one hosted, one
self-hosted, and one actively federating—use Waddle for eight consecutive weeks
without Slack or Discord as their primary fallback.

The supported client set for this program is web/installable PWA, iOS, and
macOS. Android uses the PWA. The initial adoption path is a fresh community with
templates and onboarding; Slack/Discord import, native Android, enterprise
identity/compliance suites, default end-to-end encryption, and broad AI features
are explicitly deferred.

## Program status

Status values are `planned`, `in-progress`, `blocked`, and `complete`. A gate can
be marked `complete` only when every exit criterion has durable evidence linked
from this document. Dates are assigned only after a gate has been decomposed and
sized; they are not substitutes for passing the gate.

**Current gate: Gate 0**

When all gates are complete this marker becomes `Current gate: Program
complete`; completion requires Gate 5 pilot evidence as well as a `ready`
critical-journey ledger.

| Gate | Status | Evidence |
| ---: | --- | --- |
| 0 | in-progress | [XEP conformance audit](../xep-conformance-audit.md); [server capability manifest](../../server/capabilities.toml); [critical journeys](../product/critical-journeys.json); [typed gate evidence](../product/gate-evidence.json); baseline audit pending |
| 1 | planned | Reliability SLO report, restore exercise, and multi-replica auth/reconnect results pending |
| 2 | planned | Federation profile and Prosody/ejabberd/Snikket interoperability results pending |
| 3 | planned | Shared web/PWA, iOS, and macOS journey report pending |
| 4 | planned | Pilot onboarding, moderation, audit, and administration evidence pending |
| 5 | planned | Views/catch-up adoption and extension isolation evidence pending |

## Gate 0 — Establish an honest capability baseline

- Derive the server capability inventory from typed disco declarations and
  reconcile it with dedicated XEP suites, end-to-end scenarios, web support,
  Apple support, PWA behavior, and federated behavior.
- Classify capabilities as `production`, `beta`, `experimental`, or
  `unsupported`; module existence and documentation claims do not count as
  implementation evidence.
- Define shared critical journeys for authentication, invite/join, rooms, DMs,
  history, unread state, search, files, threads, reactions, moderation,
  notifications, calls, reconnect, multi-device use, and federation.
- Instrument privacy-respecting product measures and service-level indicators
  before pilot recruitment.

Exit: the published inventory agrees with live service discovery and automated
tests, every advertised XEP has a dedicated Rust suite, and each critical
journey has an owner, supported-client matrix, and executable or explicitly
manual evidence requirements with an explicit current status. The switchable
release remains blocked until every generated scenario has complete evidence.

## Gate 1 — Make Waddle trustworthy as a daily driver

- Move authorization and temporary credential state out of replica-local
  memory into shared, expiring, atomically consumed storage.
- Prove encrypted backups, WAL archival, restore procedures, and alerting.
- Finish clustered delivery, archive, offline, notification, and reconnect
  correctness, including XEP-0352 client-state behavior.
- Keep XMPP authoritative for calls and add operational dashboards/runbooks.

Exit: messaging availability is at least 99.95%; at least 99.99% of accepted
messages are delivered or durably queued; same-region p95 send-to-visible is
under 500 ms; reconnect/resume succeeds at least 99%; authentication succeeds
at least 99.5% excluding cancellation; RPO is at most five minutes and a restore
demonstrates RTO of at most 30 minutes.

## Gate 2 — Add secure, interoperable XMPP federation

- Implement a dedicated S2S subsystem with RFC 6120 discovery and TLS,
  XEP-0368 direct TLS, SASL EXTERNAL, and encrypted XEP-0220 dialback fallback.
- Support federated DMs, roster/presence, room participation, invitations,
  archives, moderation, and capability-aware rich-message degradation.
- Add per-domain policy, resource limits, retry/dead-letter behavior, abuse
  handling, diagnostics, and administration.

Exit: bidirectional messaging, remote room participation, moderation,
reconnect, and failure recovery pass against current Prosody, ejabberd,
Snikket, and multi-node Waddle deployments without allowing a failing domain to
exhaust shared resources.

## Gate 3 — Complete the cross-platform collaboration core

- Make web/PWA, iOS, and macOS pass one critical-journey contract; the PWA must
  provide installability, Android-quality responsive behavior, offline shell,
  notifications, deep links, and safe updates.
- Complete permission-aware workspace search and dependable 1:1/room calls
  with screen sharing, participant controls, device selection, and reconnect.
- Enforce accessibility, keyboard, performance, and recovery requirements.

Exit: all critical journeys pass on desktop web, Android PWA, iOS, and macOS,
including offline/reconnect, multi-device, and federated variants.

## Gate 4 — Make communities easy and safe to operate

- Ship invite/rules/role/template onboarding and the complete member lifecycle.
- Add block/report, moderator queues, evidence and reasons, timeout/kick/ban,
  appeals, basic automod, and durable audit history.
- Permission-check and attribute every privileged action, with role-preview and
  operational health surfaces for administrators.

Exit: median invite-to-first-message is under three minutes; at least 60% of
accepted invitees engage within 24 hours; reports reach moderators within five
seconds; emergency bans propagate within two seconds; every destructive or
administrative action has a durable audit record.

## Gate 5 — Build Waddle's moat and ecosystem

- Add opt-in personal/shared views and catch-up workflows over familiar rooms,
  forums, threads, mentions, people, tags, integrations, and time windows.
- Harden feeds, stories, calendars/events, forums, and stage-style calls for
  developer communities.
- Productize the WASM extension runtime with a signed catalog, explicit grants,
  XMPP-native lifecycle management, quotas, health, audit, and crash isolation;
  ship curated RSS, GitHub, YouTube, calendar, and automation extensions.

Exit: an administrator can manage a curated extension without redeploying the
server, an extension failure cannot degrade messaging, and pilot evidence shows
repeat adoption of views or catch-up workflows.

## Delivery rules

- XMPP service discovery is the runtime source of truth. Generated inventories
  are documentation and test artifacts derived from it.
- Official namespaces and advertised features must match their XEPs in
  [`./xeps`](../../xeps); Waddle namespaces require a documented gap, typed Rust
  models, fallback behavior, exact advertisement, and dedicated tests.
- Every slice is validated at the typed parser/builder, server behavior, client
  journey, and multi-node/federated end-to-end layers appropriate to its scope.
- Gates 0, 1, and 5 use the typed immutable evidence ledger; Gates 2–4 use the
  per-scenario journey ledger. Tracker links summarize evidence but cannot mark
  a gate complete unless its machine-readable ledger is also `ready`.
- Rollout proceeds through dogfood, three design partners, and then the
  eight-week switchable beta. At least one pilot exercises supported
  self-hosting and restore procedures.
