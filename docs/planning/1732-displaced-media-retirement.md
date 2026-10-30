# Displaced occupancy media retirement (#1732)

## Claude review completion plan

The first implementation did not recover a SID learned after takeover and did
not recognize displacement of a registration restored without an occupancy
generation. Complete those paths before claiming issue acceptance.

1. Lock existing successor, departure, three-generation replay and retry behavior
   with the current SFU/Muji regressions. Add failing public-boundary regressions
   for delayed SID evidence, polling recovery, mid-call restart and genuine resume.
2. Review attribution independently: a post-takeover SID or whole-second timestamp
   alone cannot distinguish displaced media from a connected successor. Carry
   verifiable media-generation evidence or establish it before token publication.
3. Replace permanent retired-SID mode with a bounded retirement/observation model.
   Reset the successor's SID history and keep displaced evidence separately;
   preserve safety for delayed, missing-time and repeated-generation observations.
   Test expiry/reconciliation rather than assuming a short TTL proves authority.
4. Correct empty-token-bucket handling/commentary without weakening the existing
   issued-before-calls lock order or failed-CAS side-effect guarantees.
5. Move startup filesystem changes to their own prerequisite PR and restack this
   PR so its review diff contains only media-retirement work.
6. Clarify the coordinated Recreate outage, forward-recovery/rollback procedure,
   guarded separate RollingUpdate restoration and required post-deploy checks.
   Deployment itself is outside this implementation task.
7. Run focused checks after each behavior change, then independent adversarial
   review, full workspace verification and CI against the final commit.

The simplification pass is bounded to SFU participant/token observation state,
its adapter/test callers and the existing retirement outbox where required. No
new dependencies or unrelated actor cleanup are planned.

Retire media authority belonging to a replaced MUC occupancy generation while
preserving the successor's registration, tokens, and LiveKit participant.

## Implementation plan

1. Reproduce replacement through `SfuService` with a recording LiveKit admin
   boundary. Cover a successor that never connects and one that is already live.
2. Review the generation, session, participant-SID and remote-removal boundaries
   independently before changing implementation. Reuse the existing token
   publication CAS and teardown machinery where their authority fits.
3. Attribute token issuance to its occupancy generation and retire only the
   displaced generation's credentials. Preserve same-generation refreshes and
   failed publication attempts.
4. Capture displaced participant evidence before replacing the registration;
   fence remote retirement against successor connections, with explicit handling
   for missing evidence and failed admin calls.
5. Keep the existing occupant-gated departure behavior and dedicated XEP-0272
   tests green. Add regression coverage at the Muji seam where appropriate.
6. Run focused tests and type checks during implementation, then formatting,
   Clippy with `-D warnings`, structural checks, the server workspace suite and
   doctests. Review the final change independently and resolve concrete findings.

## Verification boundaries

- SFU public service: issuance, replacement, revocation and registration ownership.
- LiveKit admin boundary: the exact participant targeted by remote retirement,
  successor protection, and retry behavior.
- Muji/XEP-0272: occupancy generation flows from signaling into SFU replacement.

No new XMPP wire format is planned. The durable retirement action requires a
coordinated Recreate rollout because old outbox readers cannot decode it. Live
deployment verification is separate from native regression and CI evidence.

## Authority and limits

- Retirement has a distinct typed/persisted action. It never clears the MUC
  occupant or deletes the room. Ordinary departure session/generation gates
  remain unchanged.
- Retirement is recorded before SID discovery. An exact displaced media
  generation or a captured SID is mandatory; a generation-only action resolves
  its SID from an authoritative provider listing. An unknown or mismatched
  generation cannot authorize identity-only removal.
- Muji JWTs carry signed generation attributes and explicitly deny client
  metadata updates. Webhooks and polling preserve that evidence separately from
  MUC ownership. Restored matching-generation media survives genuine resume;
  replacement resets the successor's SID history without a permanent retired-SID
  flag or an expiring ignore set that could reauthorize delayed old events.
- Only the durable drainer executes retirement. It validates the exact current
  room claim, node incarnation and actor fence before and after provider probing.
  Unknown ownership remains retryable. A newer canonical occupant is protected
  even when the producing process retains an older cached registration.
- Tagged joins request canonical validation even after cache loss. Polling
  returns a typed inventory; the server compares it with a fresh owned snapshot
  once per room and persists only mismatched candidates. Healthy polling does
  not create a no-op outbox row per participant on every pass.
- Ghost cleanup carries the departing registration's captured generation and
  provider evidence. Old generationless webhook-ledger entries cannot perform
  an unfenced MUC presence clear.
- LiveKit's public RemoveParticipant API accepts room and identity, with no
  conditional participant-SID parameter. A successor connecting between the last
  check and that remote request remains an API limitation. These native tests
  establish protection before and during the probe, not atomic remote removal.
  Source: https://github.com/livekit/protocol/blob/main/protobufs/livekit_room.proto
- JTI revocation is Waddle's per-issuance bookkeeping; self-hosted LiveKit does
  not consult that map when accepting a token. Participant retirement removes
  existing media access; this change does not add provider token admission.
- Legacy untagged media cannot be classified safely as genuine resume versus
  replacement. The activation boundary requires draining existing SFU media and
  invalidating legacy JWTs with credential rotation. Restarting Waddle alone
  proves neither. No credentials or live deployments were changed by this work.
