# ADR-0013: Group DM Protocol Shape

## Status

Accepted

## Context

Issue #950 starts Workstream B from PRD #945: group DMs should behave like direct
messages in product placement while reusing the existing XEP-0045 room machinery
for multi-party conversation features.

The protocol needs to preserve Waddle's XMPP-native rule. Group-DM creation,
classification, membership, and follow-up sync must stay on XMPP surfaces rather
than side-band HTTP APIs.

ADR-0009 establishes relationship-based authorization as the general permission
model. Group DMs deliberately use a flatter room-level model: the service owns
the room and humans are plain members, so no human participant can kick, ban, or
moderate another participant.

## Decision

Group DMs are hidden, persistent, members-only, non-anonymous XEP-0045 rooms on
the MUC service, outside any Space.

Creation is exposed as a XEP-0050 ad-hoc command:
`urn:waddle:group-dm:create:0`.

Provisioned rooms advertise the Waddle classification feature
`urn:waddle:group-dm:0` in room `disco#info`. Clients classify rooms with that
feature into the DM sidebar, not the channel/sidebar Space hierarchy.

The first walking-skeleton slice creates the room, writes the creator and
initial participants as member affiliations, keeps the room hidden from public
MUC discovery, and exposes the classification feature. Mediated invites and
XEP-0402 autojoin bookmarks remain part of the same protocol shape and should be
implemented as follow-up slices on the same command.

## Consequences

### Positive

- Existing MUC message features can be reused for replies, reactions, threads,
  uploads, read markers, unread counts, and pins.
- Client classification has a stable disco feature instead of relying on JID
  naming conventions.
- The flat human-member model avoids accidental human moderation authority in
  private group conversations.

### Negative

- Group-DM creation now has its own Waddle namespace because no XEP defines this
  exact provisioning command.
- The first slice does not complete cross-device appearance until XEP-0402
  bookmark publication is added.

## Related

- [ADR-0009: Zanzibar-Inspired Authorization](0009-zanzibar-permissions.md)
- [Issue #950: Group DMs walking skeleton](https://github.com/waddle-social/waddle/issues/950)
