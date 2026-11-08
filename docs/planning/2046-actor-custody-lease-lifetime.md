# #2046 actor custody lease lifetime

## Objective

Keep the existing serialized-envelope count and byte charge reserved for as
long as any shared handle can retain the original `OutboundStanza`. Preserve
the actor custody state machine, exact pre-acceptance refusal, callbacks,
metadata and wire behavior. Make a same-owner replacement refuse to publish a
new binding while a prior generation still has outstanding leases.

## Planned change

- Put the original outbound envelope and a non-cloneable count/byte charge in
  one opaque shared `Arc` lease.
- Let custody records and snapshots share that lease. Terminal record removal
  changes inventory and stage as before, while the charge is released only
  when the final lease handle is dropped.
- Keep `retained_count()` as the number of custody records. Track outstanding
  leases separately for byte/count admission and replacement gating.
- Before `actor_local_endpoint::new_binding_channel()` clears sealed prior
  custodies or creates the next generation, require both zero record inventory
  and zero outstanding leases. This first change refuses replacement while an
  old snapshot is held; it does not share a budget across generations.
- Keep the current serialized XML plus envelope-metadata charge calculation.
  This change does not claim to bound the typed stanza's heap size or to make
  the actor endpoint active for `/ws`.

## Acceptance criteria

- One-slot tests prove count and byte capacity remain unavailable after
  terminal removal while a snapshot is held, then become available after the
  last shared handle drops.
- A replacement-generation test proves a held snapshot prevents a new binding
  channel; dropping the last snapshot permits the existing lifecycle to
  continue.
- Refusal returns the exact original envelope, including pending identifiers,
  ingress metadata and write-acceptance handles.
- Existing queued, attempting, uncertain, flushed and sealed transitions keep
  their current behavior under snapshot and drop races.
- No XMPP wire shape, SM replay identity, pending-delivery state, feature
  advertisement or workflow changes.
- Run targeted custody and replacement tests, strict Clippy for affected Rust
  crates, and formatter checks after the shared Cargo slot is released. Obtain
  a read-only exact-head review before handoff.

## Scope limits

The lease represents the current serialized-byte accounting model. It is not a
proof of actual `Stanza`/minidom heap bytes, does not bound temporary typed or
writer copies, and does not change the live legacy `/ws` queue selection.
