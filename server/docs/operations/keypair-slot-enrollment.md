# Keypair slot enrollment (dark path)

The keypair slot owner API records one ordered `slot_index -> PeerId` map for
one generation. Runtime attachment checks that exact order in both attested
main and control-plane pools before it can acquire a slot. A pending generation
cannot attach. The active map is protected by database triggers against
ordinary row mutation; the restricted runtime role can only read the map and
read, insert, or update lease rows.

This is a dark path. Swarm startup still uses the legacy lease constructor and
does not call `attach_existing` or the owner enrollment API. Do not treat the
schema or tests as runtime activation.

## Required rollout fence

Enrollment only creates a fresh owner-controlled schema contract. The owner
initializer refuses a schema containing a legacy lease table, keypool table,
or one of its guard objects. It does not migrate or adopt them.

Before using this path in a deployment, stop every old and new clustering
process and prevent autoscaling or restart automation from bringing any back.
Keep the full stop in place for at least the maximum configured lease TTL plus
an operator-selected margin. Confirm no node still holds a lease. This
full-stop and drain check is external to the code and is not inferred from an
empty table or an elapsed delay. Do not roll back to a binary that uses the
legacy keypair slot SQL after owner enrollment: old inserts omit required
generation fields and old updates fail the revision trigger.

## Enrollment sequence

1. Generate the private keypool with `waddle-cluster-keypool generate` and
   retain its output securely. Keep the keypool order unchanged wherever it is
   configured. Record the corresponding ordered PeerIds and a fresh UUID for
   this enrollment generation in the deployment change record. Never log or
   publish private seeds.
2. With the old fleet stopped, use a non-attested owner `Database` handle for
   the exact enrolled PostgreSQL schema and call
   `PostgresKeypairSlotLease::initialize_keypool_owner` with the schema's
   `PgIdentity`, the generation UUID, the ordered PeerIds, the restricted
   runtime role name, and a drain delay at least as long as the required
   full-stop window. The owner role must be able to execute
   `pg_catalog.pg_control_system()` so the shared owner-boundary check can
   attest the physical PostgreSQL identity. This creates a pending generation
   and grants only the map-read and lease DML privileges required by the runtime
   role.
3. Keep the generation pending until the external full-stop, lease drain, and
   delay have all been independently confirmed. Then call
   `PostgresKeypairSlotLease::activate_keypool_owner` using the same owner
   handle, identity, and generation. Activation also verifies that the map is
   contiguous, has the declared number of slots, and that no lease row has
   already been created.
4. Before any future startup wiring, construct the attested runtime `Database`
   and call `PostgresKeypairSlotLease::attach_existing` with the exact ordered
   base64 entries from `WADDLE_CLUSTERING_KEYPAIR_POOL`. It derives the PeerIds
   from those secrets and verifies both physical
   pools, schema and relation shape, role ACLs, owner guards, active generation,
   and exact PeerId order. A mismatch must stop startup before lease acquisition
   or swarm identity use.
5. Keep the fleet stopped through configuration rollout. Every node must use
   the same ordered keypair pool. A reversed or otherwise changed order is
   refused by attachment, even when it contains the same set of PeerIds.

## Rollback and limits

The generation is forward-only. The active map cannot be changed through
ordinary DML; re-enrollment requires a separately designed owner operation and
another full fleet stop. Do not delete the prepared relations or reuse an old
generation UUID as a rollback procedure. There is no automatic rollback to
legacy lease SQL.

This implementation does not stop old processes, prove their absence, fence
deployment automation, or wire the attach check into swarm startup. Those are
deployment and later integration requirements. The generated allowlist SQL is
also separate from the owner keypair mapping; do not assume it enrolls this
generation.
