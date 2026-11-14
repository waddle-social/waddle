# SpiceDB authorization schema versions (#2047)

The server's built-in SpiceDB schema
(`server/crates/waddle-server/src/permissions/schema.rs`) is versioned. Before
this release every replica rewrote the schema at every start, so a replica of
an older release reverted any newer schema it found. Now a replica writes the
schema only to install it into an unversioned store or to upgrade a store
recorded at an older version, and refuses to start against a store recorded at
a newer or unrecognised version without writing anything
(`permissions::authz_schema_version`).

The version is recorded in one marker relationship:

```
waddle_authz_schema:installed#version_1@waddle_authz_schema:installed
```

The relation name carries the version, and the schema of version N declares
only `version_N` in `waddle_authz_schema`. SpiceDB refuses a schema write that
would remove a relation or definition that still holds relationships. Once the
marker exists, a binary whose schema lacks that relation cannot overwrite the
schema, whatever its own startup logic: releases before this one lack the
whole `waddle_authz_schema` definition. The version is read from this
relationship at full consistency, never by comparing schema text, because
`ReadSchema` can serve a cached schema after a direct datastore reset.

## Version 1: outcast precedence

Version 1 is the first versioned schema. It changes behaviour: every channel
permission (`read`, `send_message`, `view`, `moderate`, `manage`, `delete`)
subtracts `outcast`, including what a subject holds through the parent Space
(`parent->view`, `parent->moderator`, `parent->admin`), and so do a message
author's own `edit` and `delete` (`author - channel->outcast`). Before it, a
room outcast kept every channel permission that another relation or the parent
Space granted, and kept control over their own messages.

For a subject holding `channel#outcast`, SpiceDB now answers:

- every channel permission: denied, whatever direct channel relation (`owner`,
  `admin`, `member`, `manager`, `moderator`, `writer`, `viewer`) or Space
  relation (`member`, `moderator`, `admin`) the subject also holds. A Space
  admin banned from a room can no longer manage, moderate or delete that room
  through the Space; their Space permissions are unchanged;
- messages in that channel: `read` and `react` denied (they are
  `channel->read`), and a banned moderator can no longer delete other authors'
  messages (`channel->moderate`);
- the author's own `edit` and `delete` on their messages in that channel:
  denied. Moderators of the room who are not banned can still delete those
  messages through `channel->moderate`.

The server code paths that serve rooms today already check `outcast` before
these permissions: MUC join and archive access, the extension host, extension
PubSub commands and extension route items. For them the visible result does not
change; the schema now enforces the same rule for every caller, including any
that does not check `outcast` first. The local test backend evaluates the same
rule.

## Startup

At startup, after the codec marker pre-check of
[authz-codec-cutover.md](authz-codec-cutover.md):

1. The replica reads the version marker.
   - Version 1: it proceeds without writing the schema.
   - None: it writes the built-in schema.
   - An older version: it writes the built-in schema with the older marker
     relation kept beside its own.
   - A newer version, or any other content: it refuses to start and writes
     nothing.
2. The codec guard binds or refuses the store.
3. The replica records the version: it creates the marker if there is none, or
   replaces an older marker and then writes the built-in schema again to drop
   the older relation. Replicas starting together record one marker. It logs
   `Recorded the authorization schema version` or `Upgraded the authorization
   schema version`.

The marker is recorded only after the codec guard accepts the store. A store
the codec guard refuses gains no relationship, so the release that wrote it can
still start against it.

## Rollout within #2047

Version 1 is first installed by the same start that binds the empty store to
the `v2` ID codec. The codec cutover already requires every replica of the
previous release to be stopped and the store to be empty, so there is no
mixed-version period for this transition. Follow
[authz-codec-cutover.md](authz-codec-cutover.md), and in its verification step
also check:

- `zed relationship read waddle_authz_schema` returns exactly the marker above;
- one replica logged `Recorded the authorization schema version`;
- bans re-applied after the cutover deny the banned account in the room, also
  when the account is a member of the parent Space.

Once the marker exists, a replica of the previous release fails at startup:
its schema write would remove `waddle_authz_schema` (and `waddle_authz_codec`),
and SpiceDB refuses it. Replicas of the previous release that are still running
cannot exist at this point, because the codec cutover stopped them; if one
does, it evaluates every check against the version 1 schema, since SpiceDB
evaluates permissions centrally.

## Later schema versions

A schema change ships as a new version: bump `AUTHZ_SCHEMA_VERSION`, rename the
`version_N` relation of `waddle_authz_schema` in the built-in schema, and update
the pinned digest in `server/crates/waddle-server/tests/authz_schema_version.rs`,
which fails on any schema change without a new version.

Mixed-version replicas during such a rollout:

- SpiceDB evaluates permissions centrally, so every replica, old or new,
  answers under whichever schema is stored. From the moment the first new
  replica writes its schema (step 1), old replicas serve under the new policy.
  If the new policy is not safe for the old binary's callers, roll out with
  `Recreate` instead of `RollingUpdate`.
- Until the new marker is recorded (step 3), an old replica that starts reads
  its own version, proceeds and writes nothing.
- After the new marker is recorded, an old replica that starts refuses with
  `authorization store schema is version N, newer than version M of this
  server`, and SpiceDB would refuse its schema anyway. Already running old
  replicas keep serving under the new schema until they are replaced.

Old-binary retirement: the rollout is complete when no pod of an older image
remains. A crash-looping old pod that logs `Refusing the authorization store:
its schema version is not one this server may write` is an old binary that was
not retired; remove it, do not work around the refusal.

## Rollback

Prefer roll-forward. A binary rollback within the same schema version needs no
store action. A binary rollback to an older schema version is refused at startup
by design. **Do not delete or edit the marker to get past the refusal**:
without it, the older release installs its own schema and reverts the newer
policy, for version 1 outcast precedence, for every replica at once.

If a schema downgrade is explicitly authorised, it is an operator action with
the fleet stopped: stop every replica of the newer release, delete the marker
(`zed relationship delete waddle_authz_schema:installed version_N
waddle_authz_schema:installed`, syntax to be confirmed against the deployed
`zed` version), then start the older release, which installs and records its
own schema. Rolling back past this release in #2047 is the reverse codec
cutover of [authz-codec-cutover.md](authz-codec-cutover.md), which empties the
store including both markers.

## Refusals

Each refusal is logged as `Refusing the authorization store: its schema version
is not one this server may write` with a `reason`, and fails startup:

- `authorization store schema is version N, newer than version M of this
  server`: a later release recorded the store. Retire this binary, or run the
  release that recorded it. Startup fails with `Failed to ensure permission
  schema: Authorization store refused: ...`, or with `Failed to record the
  permission schema version: ...` if a later release recorded the store while
  this replica was starting.
- `authorization store holds an unrecognised schema-version marker`: the
  marker definition holds anything other than one well-formed marker. Do not
  edit it; find out what wrote it.
- `authorization store schema version changed while it was being recorded`:
  another writer changed the marker while this replica recorded it. Restart;
  the next start decides again.

The local permission backend has no stored schema and is not affected; it is
selected only in the test configuration described in
[authz-codec-cutover.md](authz-codec-cutover.md). These instructions do not
authorize or assert a deployment, a store deletion or a rollback.
