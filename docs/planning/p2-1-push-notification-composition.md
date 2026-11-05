# P2.1 Push and notification startup composition

This #2046/#2116 slice prepares the Push and notification family to select
the physically attested main pool. It remains dark: restricted startup still
returns `UnconvertedStores` before listeners and constructors.

## Plan

1. Obtain an independent architecture gate for the joint Push registration,
   VAPID key, Push Service, notification outbox, notification activity,
   PubSub-backed settings and DND boundary. Challenge key custody, owner
   preparation, ancestry adoption, runtime ACL, transaction and cancellation
   semantics, old-writer exclusion, and every constructor's startup side
   effects.
2. Add owner-only preparation and read-only runtime attachment for VAPID key
   storage. Runtime attachment must verify the exact main-pool identity,
   relation/index/ACL shape and prepared key state, then load a signer without
   provisioning, repairing, or reading another pool.
3. Add a dark composite selector that attaches each already prepared Push and
   notification store on the same attested main handle. It must skip owner
   ancestry adoption, schema DDL and key provisioning. Preserve legacy
   SQLite/PostgreSQL startup behavior and the PubSub settings/DND handle.
4. Verify PostgreSQL 17 owner/runtime role separation, same-pool one-slot
   admission, prepared and missing states, role/schema/ACL drift, no runtime
   writes or child pools, cancellation and legacy compatibility. Run format,
   scoped strict Clippy and independent final review.

If the architecture gate does not clear functional composition, keep the
affected pieces as isolated dark verifiers and record the exact prerequisite.
No worker, Push command, XMPP wire, epoch, mailbox or serving activation is in
scope. Owner-side adoption and old-writer cutoff must complete before a later
restricted serving cutover.

## Architecture outcome

The independent gate cleared only dark, read-only attachment and an opaque
diagnostic over the same attested main handle. It did not clear serving or a
functional Push bundle. VAPID needs owner-prepared key custody and a separate
read-only attach with qualified SQL, exact role/ACL/schema checks, a durable
completion proof, and public/private key consistency. The diagnostic may
observe the existing store attachments; it must not expose their functional
handles to startup. Before serving, a joint cross-store ACL and custody proof
must cover Push registration and notification outbox writes, owner ancestry
adoption, the old-writer cutoff, and root-key rotation policy. The fatal
startup guard remains in place throughout this slice.
