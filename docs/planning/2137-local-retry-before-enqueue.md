# P3.1 local retry-before-enqueue custody

Status: bounded dark implementation slice for [#2137](https://github.com/waddle-social/waddle/issues/2137). This work prepares one local egress boundary for future provisional endpoint use. It does not activate endpoint authority.

## Goal

Carry a proven pre-enqueue rejection from the local `UserActor` direct/peer send boundary back to its caller with the exact `OutboundStanza`, local binding owner, and `BindingRef`. A caller can then retry the same envelope against the same binding after activation without reconstructing or retargeting its obligations.

## Scope

- Add a typed local result for `ConnectionEntry` archive dispatch that preserves the outbound envelope when the exact endpoint refuses it before queue acceptance.
- Add dark direct/peer `UserActor` entry points that preserve the typed refusal and exact binding identity. Existing fire-and-forget APIs retain their current behavior.
- Test that refusal leaves the socket queue and archive frontier unchanged and returns the same callback, pending-row metadata, receipt time, and ingress obligation.
- Test that a refusal remains bound to the original local owner and `BindingRef`, including same-JID replacement.

## Out of scope

- Do not construct or publish provisional entries from production registration, seal production bindings, or activate endpoint authority.
- Do not retry through another resource, offline storage, or a successor binding.
- Do not change remote-resource proxies or relay message IDs, pending-flush handling, ordered-relay behavior, storage schemas, or XMPP wire behavior and feature advertisements.
- Do not claim acceptance or recovery custody after queue admission.

## Acceptance

- A provisional, sealed, or exact-binding mismatch refusal returns the original `OutboundStanza` and both local owner and `BindingRef`.
- No refused frame reaches the receiver, and no archive frontier advances.
- The active-path result and existing suppression behavior remain unchanged.
- Focused `waddle-xmpp` tests, formatting, and strict Clippy pass.

This slice is one local API prerequisite only. Remote proxy gating, other producer paths, accepted-but-unwritten custody, endpoint storage/CAS, and the coupled P2/P3 activation gates remain open.
