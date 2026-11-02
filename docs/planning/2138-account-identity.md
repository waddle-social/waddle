# P3.2 account-resource identity

Status: draft dark implementation plan for #2138 and #2046, stacked on #2150.
This slice does not switch production resource selection or delivery.

## Identity boundary

A full JID is a replaceable alias. Every account-resource mutation must compare
an exact `UserClaimIdentity`, `BindingRef`, expected revision and state. The
claim identity contains the bare JID, the complete node lease or standalone
process incarnation, the claim epoch and a fresh account-actor incarnation.
The actor incarnation prevents same-owner self-reacquisition of a claim from
authorizing a stale actor. A local binding ID is not a remote source-attempt ID;
failed resume and fresh bind on the same physical connection need distinct
`BindingRef` values.

The first dark slice exposes and tests these typed identities and an isolated
compare-and-set resource repository. It must not add a second production writer.
The current UserActor selection/delivery, ConnectionRegistry presence mirror,
and UserEntry cleanup mirror stay in their current roles until a reviewed
single-writer handoff.

## Remote registration

Existing register v4, unregister v2 and update v1 message IDs remain frozen.
A future identity-bearing register v5 request carries the source `BindingRef`;
the accepted reply returns the complete `UserClaimIdentity`. Later update v2
and unregister v3 must carry that identity and the exact binding, registration
and socket-generation tokens. An identity-sensitive operation must fail closed
against an old peer; it cannot fall back to the older message versions. These
are server-to-server messages, not a change to the XMPP client protocol.

## Acceptance and activation gates

- Same full-JID concurrent binds, failed-resume/fresh-bind on one socket,
  delayed old register/update/unregister and actor replacement cannot mutate
  a successor.
- Real PostgreSQL compare-and-set races prove the complete claim and binding
  identity in the write transaction. A separate advisory claim check is not a
  substitute for that fence.
- Versioned codec fixtures prove old messages stay unchanged and a new peer
  never retries a required-identity operation through an old wire ID.
- P2.1 typed transaction authority, P3.1 connection identity and P3.3 durable
  session-generation contracts are prerequisites for live repository writes.
  #1986/#2118/#1948 also constrain affected delivery cutovers.

Keep the repository and new wire operations unwired until account resource
selection, metadata, cleanup and routing have one authoritative owner, and the
joint P3.1/P3.2/P3.3 activation gate is qualified.
