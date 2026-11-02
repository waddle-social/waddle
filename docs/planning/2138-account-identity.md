# P3.2 account-resource identity

Status: dark identity and transition fixtures for #2138 and #2046, stacked on
#2150. This slice does not switch production resource selection or delivery.

## Identity boundary

A full JID is a replaceable alias. Every account-resource mutation must compare
an exact `UserClaimIdentity`, `BindingRef`, expected revision and state. The
claim identity contains the bare JID, the complete node lease or standalone
process incarnation, the claim epoch and a fresh account-actor incarnation.
The actor incarnation distinguishes actors even when same-owner
self-reacquisition keeps the claim epoch; it fences a stale actor only after
the claim lifecycle installs and compares the complete identity. A local
binding ID is not a remote source-attempt ID;
failed resume and fresh bind on the same physical connection need distinct
`BindingRef` values.

This slice exposes typed identities, reserved remote wire shapes and a bounded
in-memory transition fixture. The fixture assumes an external authority has
installed each claim. It does not establish that a claim is current, implement
a database compare-and-set repository or add a production writer. The current
UserActor selection/delivery, ConnectionRegistry presence mirror, and UserEntry
cleanup mirror stay in their current roles until a reviewed single-writer
handoff.

## Remote registration

Existing register v4, unregister v2 and update v1 message IDs remain frozen.
The new message IDs are reserved but have no registered handlers or senders.
A future identity-bearing register v5 request carries the source `BindingRef`;
the accepted reply returns the complete `UserClaimIdentity`. Later update v2
and unregister v3 must carry that identity and the exact binding, registration
and socket-generation tokens. An identity-sensitive operation must fail closed
against an old peer; it cannot fall back to the older message versions. A
future handler must validate `socket_node` and the binding's node lease against
the authenticated sender and lease. These are server-to-server messages, not a
change to the XMPP client protocol.

## Acceptance and activation gates

- The inactive fixture characterizes serialized same full-JID binds,
  failed-resume/fresh-bind on one socket, stale updates and retirement, actor
  replacement, operation replay and unknown-result lookup. Its externally
  supplied claim replacement and custody assumptions are not live evidence.
- Named MessagePack fixtures freeze old registration bytes and check the new
  identity-bearing shapes. New peer dispatch and fail-closed retries remain
  unimplemented and require separate tests before activation.
- Before a durable repository, settle how P3.1 `BindingRef` and endpoint
  generation relate to P3.3 logical session and SM identity, including detach,
  resume, compensation and optional exact references. The current claim table
  lacks actor incarnation; same-node claim reacquisition can keep the epoch.
  Installation, rotation, revocation and exact release of actor identity need
  one authoritative transaction contract.
- P2.1 typed transaction authority, the epoch guard and global lock order must
  cover the claim and resource mutation together. Operation identity, payload
  matching, commit-unknown lookup, retention and recovery must be fixed before
  schema/API freeze. Real PostgreSQL concurrency and multiprocess tests are
  then required; SQLite serialization proves only local behavior. #1986,
  #2118 and #1948 also constrain affected delivery cutovers.

Do not add `user_resources`, endpoint or SM foreign keys, or a second operation
journal while those contracts are unresolved. Keep the new wire operations
unwired until account resource selection, metadata, cleanup and routing have
one authoritative owner and the joint P3.1/P3.2/P3.3 activation gate is
qualified.
