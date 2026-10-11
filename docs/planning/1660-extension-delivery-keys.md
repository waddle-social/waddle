# Opaque extension effect delivery (#1660)

Implementation plan reviewed by `gpt-6-astra` at high reasoning on 2026-10-09.
Prerequisite #1658 is closed; its bounded uncertain-send policy remains the baseline.

## Contract

Supported activity, preview, notification and extension effects keep a canonical
delivery identity and their approved payload through replay. Local mutation and
completion commit together, or a downstream accepts the same key durably. The
Foundation unit of work and existing effect receipts remain the sole authority.
Unknown provider outcomes remain retryable and duplicate-possible; a stable
XEP-0357 item ID does not prove provider acceptance. Calls and pins retain the
`AwaitingDurableOwner` carve-out.

## Implementation lanes

1. Bind opaque keys to canonical effect/target authority, reuse existing receipt
   and transaction interfaces, and define retention eligibility under the same
   canonical lock. Append any schema changes through the checksummed ledger.
2. Execute frozen activity and preview mutations plus receipts transactionally;
   reconstruct preview recovery from stored intent without rerunning enrichment.
3. Carry host-owned opaque capabilities through the extension invocation ABI.
   The host retains Foundation bindings; guests cannot export, construct or forge
   keys. Reuse durable observer work and approved publications, and preserve
   explicit uncertainty for arbitrary guest/provider calls.
4. Carry canonical ancestry through notification candidates, coalesced jobs and
   durable push acceptance. Preserve keyless provider at-least-once semantics and
   remove identity-bearing transition telemetry in touched paths.
5. Retain aliases, keys and receipts until descendants and SM references settle,
   then retain the eight-day tail. Reference changes and GC must serialize;
   transport acknowledgements are not retention eligibility.

Independent code lanes will use explicit file ownership. Shared routing,
migrations and ABI integration must be coordinated before integration.

## Test seams and acceptance

Use the effect executor/UoW, notification queue, actual extension runtime and
canonical retention interfaces already identified in issue triage. Add one
failing behavior test before each implementation slice.

- SQLite and PostgreSQL: activity/preview mutation and receipt commit or roll
  back together; same-key replay and reconstruction preserve newer state and
  approved payloads; different targets remain distinct.
- Extension runtime: host-issued capability is opaque and invocation-scoped;
  foreign/stale handles cannot settle another obligation; a guest return is not
  proof of an arbitrary external provider effect.
- Notifications: lost acceptance replies reuse durable same-key work; stable
  PubSub item identity leaves provider acceptance unproved. Unknown sends retain
  honest retry/duplicate behavior.
- GC: pending descendants and SM references preserve dedup evidence; clearing
  them starts the full eight-day tail; concurrent reference changes remain safe.
- Keep calls/pins carve-outs and existing observer, XEP-0357, #1316 and
  #1294/#1389 regression coverage intact. No identity-bearing telemetry.

## Delivery and verification

Run focused red/green tests and cargo checks throughout. At integration run
formatting, Clippy with `-D warnings`, the workspace/all-targets locked nextest CI
suite, relevant all-feature XEP/server coverage, and doctests. PostgreSQL tests
must execute rather than skip. Review the tested commit independently, resolve
actionable findings, update the PR evidence, mark ready and monitor CI.

No live deployment or activation is part of this implementation. Update
`TODO-ACTOR.md` narrowly with issue disposition and verification boundaries.
