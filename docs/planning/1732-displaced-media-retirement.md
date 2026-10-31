# Displaced occupancy media retirement (#1732)

## Review correction plan (0d1a200f8)

1. Reproduce an old, generationless queued departure through the real drain
   before changing the shared unknown-SID leave guard. Cover the matching
   Jingle-to-outbox-to-drain path and explicit hangup/ghost convergence.
2. Retry preparation once before fresh authorization, use room-filtered provider
   lookup, and report preparation failure separately from membership denial.
3. Log retirement outcomes, correct provisional-publication documentation and
   document exact destructive rollback disposal without touching unrelated jobs.
4. Refresh onto main, run relevant native/HTTP/SQL checks, independently review
   the integrated result and verify final-head CI. Do not merge/deploy.

Keep this pass inside these seams; do not add another generation state or
expand into a general actor refactor. Retain actor-only cleanup checks around
awaits; delete the duplicate check in the same synchronous post-snapshot span.

## Contract

Preserve cached-token tab reload until an authorized fresh Jingle publication
explicitly replaces that media. A different XMPP occupancy generation alone
never proves a media takeover. Ordinary joins and polling update observation
state; they do not create retirement work.

At takeover, revoke the displaced logical generation's tracked tokens and save
an exact media-generation/SID obligation. The durable action never clears MUC
occupancy or deletes the room. Its provider fences protect a connected successor,
including a successor reconnecting with cached credentials under a newer XMPP
occupancy generation.

## Cleanup and implementation plan

1. Lock reload, late-SID, restored registration, failed publication CAS,
   ownerless drain and connected-successor behavior with regression tests.
2. Remove join/poll retirement production, its inventory API and redundant
   canonical-owner callback machinery.
3. Charge the existing Muji control-plane budget and reject non-members before
   provider preparation. Recheck current role/occupancy after preparation; retain
   the separate five-per-window publication limit. Check room incarnation
   around the participant probe, then seed only under the original registration
   revision and observation-freshness checks. Retry failed probes or conflicting
   observations. Provider evidence never grants logical occupancy.
4. Use the returned expectation for the existing synchronous publication CAS.
   Stage an opaque unpersisted receipt under its call-entry guard, before exposing
   the successor binding, and dispatch the reporter only after releasing locks.
   Before releasing a replacement JWT, snapshot pending/in-flight receipts and
   await confirmed outbox inserts; acknowledge only matching receipt IDs after
   commit. Revalidate the minted role/capabilities and generation after this
   database await; failures suppress the token and roll back only that publication.
   A successful provisional CAS may dispatch retirement before finalization.
   Attribute remote retirement to observed media and local JTI revocation to logical
   occupancy; those generations can differ after cached-token recovery.
5. Drain explicit retirement on any node, even after the room owner disappears.
   Resolve generation-only jobs from LiveKit, match captured SIDs/incarnations,
   and let only observations newer than the probe veto it as a successor.
6. Capture ghost revisions before remote probes. Follow a completed SFU sweep
   with actor-only Muji cleanup, never a second SFU removal. Guard actor awaits
   against a new registration and use current actor occupancy for cached media.
   Preserve actor-only retry responsibility in the existing durable outbox.
   Revoke active-history FIFO evictions until expiry, and preserve signed events
   with malformed optional generation tags as unattributed observations.
7. Verify focused tests, SQLite/Postgres migrations, Muji integrations, strict
   Clippy, formatting, structure and independent adversarial review. Record CI
   and live rollout evidence separately.

The cleanup stays within the existing media implementation and its adapters and
regressions. No dependencies, client protocol changes, additional ownership
abstractions or unrelated actor cleanup are required. Startup filesystem work
was split into independent PR #2058 and has merged.

## Authority and failure boundaries

- JWT attributes carry the server-signed `waddle.occupancy_generation` and deny
  client metadata changes. The attribute identifies media, never MUC authority.
- Preparation also runs against an empty cache after restart. Room lookups are
  filtered to the requested name. One retry covers provider failure, room
  rotation or benign observation changes; the original signaling/occupancy
  anchor must remain unchanged across both attempts. A newer signaling binding
  supersedes the operation. Exhausted preparation emits `media_preparation_failed`
  before dispatch, separately from membership denial. The later publication CAS
  prevents authorization snapshots overwriting a newer
  registration. A failed CAS stages no retirement. A later failed finalization
  can still leave retirement durably queued: the provisional CAS already
  displaced the old registration, and the reporter may have committed it before
  final role/connection validation failed. The held replacement credential is
  suppressed and its exact publication rolled back; unacknowledged setup is
  indeterminate, not an all-or-nothing rollback of earlier retirement.
- Exact takeover responsibility is durable before SID discovery. It is never
  an instruction to remove whichever generation differs from a future occupant.
  A later matching provider listing resolves a generation-only obligation.
- Retirement requires no live room actor or claim. Its authorized publication
  already established displacement. Incomplete SID evidence uses the bounded
  ordinary retry policy; unknown/mismatched generation is a safe terminal no-op.
  Provider outages retain the existing infrastructure retry policy.
- Matching-generation media survives genuine resume. Replacement resets SID
  history rather than entering a permanent retired-participant mode. Cached
  credentials explicitly displaced by a newer Jingle setup are no longer valid
  recovery credentials; cache replacement follows successful call setup.
- Ordinary departure and room-finished cleanup retain their existing lifecycle
  and caller-held occupancy/session fences. An unscoped leave cannot supply the
  first SID for a bound, still-unobserved registration, even if it carries a SID
  or a matching tag. This also rejects queued departures whose tags were lost.
  A genuine leave before its join observation waits for the existing grace and
  two-absence-pass ghost sweep; the caller's own scoped hangup remains immediate.
  Provider tags do not stand in for
  the current XMPP occupancy: cached media can legitimately carry an older tag.
  Ghost registration revisions are captured before provider probes internally.
  Their remaining actor cleanup never removes SFU state a second time; its
  actor-only durable retry is superseded by any new registration or live
  provider media. Provider occupancy also prevents a cold cache from ending a
  different connected participant's call thread. Completion retry failures keep
  the actor-only responsibility until they can be handed off durably.

## Limits and rollout

A same-generation publication inside the room actor's clear mailbox interval
can still race its advertisement, as in the existing departure path. Actor-only
ghost cleanup suppresses its broadcast/completion when it sees the new SFU
registration and never revokes that registration's token. Completion retains
the existing helper's check-to-inbox/broadcast synchronization window; this
change does not atomically coordinate provider, actor and inbox state.

LiveKit RemoveParticipant takes room and identity, not an atomic expected SID.
A successor connecting between the final probe and that request can still be
removed. Source: https://github.com/livekit/protocol/blob/main/protobufs/livekit_room.proto
Follow-up for generation-specific provider identities and explicit old-token
admission policy: https://github.com/waddle-social/waddle/issues/2095

Local JTI revocation is bookkeeping: self-hosted LiveKit does not consult it at
admission. A completed retirement is not permanent provider token revocation or
continuous generation enforcement. Expiry bounds initial token use, not an
already-connected participant, whose refresh tokens are rotated in-band.

Untagged legacy media retains its previous lifecycle. Missing or malformed
optional generation evidence never justifies a generation-based removal. Do not
claim strict attributed retirement for legacy media or that its connected media
will disappear when an initial ten-minute token expires.

The narrower scope needs neither forced SFU draining nor signing-key rotation.
The new outbox action still cannot be decoded by old readers, so Recreate must
stop old readers before new writers. Main auto-deploys: use the authorized
rollout window and live acceptance procedure in `docs/operations/relay-cutovers.md`.
Restore RollingUpdate separately after fleet verification. Prefer roll-forward;
binary rollback requires explicit handling of incompatible rows. No live
cutover, credential rotation or live acceptance is asserted by native tests.
