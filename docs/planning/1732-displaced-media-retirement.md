# Displaced occupancy media retirement (#1732)

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
- A captured old participant SID is mandatory. The executor always probes live
  LiveKit occupancy and checks local fences again after the probe. Missing SID
  evidence cannot authorize identity-only removal; remote retirement without a
  captured old SID remains deferred.
- LiveKit's public RemoveParticipant API accepts room and identity, with no
  conditional participant-SID parameter. A successor connecting between the last
  check and that remote request remains an API limitation. These native tests
  establish protection before and during the probe, not atomic remote removal.
  Source: https://github.com/livekit/protocol/blob/main/protobufs/livekit_room.proto
- JTI revocation is Waddle's per-issuance bookkeeping; self-hosted LiveKit does
  not consult that map when accepting a token. Participant retirement removes
  existing media access; this change does not add provider token admission.
