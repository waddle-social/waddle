# SpiceDB authorization ID codec cutover (#2047)

This release changes how every SpiceDB object and subject ID is written. IDs
are now `v2_` followed by unpadded base64url of the identifier's exact bytes
(`server/crates/waddle-server/src/permissions/authz_id.rs`). The previous
release substituted `@` with `|` and `.` with `=`, which made distinct
identifiers collide, and some of its IDs are also valid `v2_` IDs: the legacy
channel `v2_c2t5` reads as the new encoding of `sky`. Relationships written by
the previous release are therefore ambiguous. They are never decoded, migrated
or read through a fallback.

At startup, before it writes the schema and before it serves anything, the
server reads the codec marker
`waddle_authz_codec:incarnation#codec@waddle_authz_codec:v2`
(`permissions::authz_incarnation`):

- marker for `v2`: the server starts;
- no marker and no Waddle relationships: the server writes the marker, then
  starts. Of several replicas starting together, one writes it;
- no marker but Waddle relationships: the server refuses to start;
- any other marker content: the server refuses to start, before it rewrites the
  schema.

Production runs the `spicedb` SpiceDBCluster on its PostgreSQL datastore, which
the running release has populated, including through the startup reseed of
deployment memberships. **This build refuses to start against production until
that store is emptied.** Each replica logs `Refusing the authorization store: it is not bound to the current ID codec`
with a `reason` naming the first populated definition, and startup fails with
`Authorization store is not usable with this server: Authorization store
refused: authorization store holds ...`. No relationship is written in that
case; the schema gains the empty `waddle_authz_codec` and `waddle_authz_schema`
definitions, which the previous release removes again when it starts.

Main auto-deploys. Merge this change only inside an explicitly authorised
`Recreate` window, or ship it as part of the planned fresh production redeploy
onto an empty SpiceDB datastore. The committed HelmRelease uses `Recreate` with
two replicas. Expect an XMPP connection interruption.

## Accepted loss

Emptying the store deletes every grant. After the cutover the server's startup
reconciliation re-creates only the deployment-level `server` owner and member
grants of existing accounts, from `WADDLE_SERVER_OWNER_LOCALPARTS` and the
account tables.

Channel `parent` links of the built-in managed channels are **not** reliably
re-created at startup. Startup writes them only when the channel's bookmark is
absent from every Spaces PubSub node (`server/topology.rs`). PubSub lives in
SQL and survives the cutover, so on an existing deployment the bookmarks are
present and the links are skipped. A missing `parent` link is repaired lazily,
when a user joins a bookmarked channel and is not already admitted by another
route (`restore_space_parent_tuples_for_room`, in
`routes/websocket/handlers/presence/muc/access.rs`). That repair is skipped for
an open (not members-only) room when the joiner is a deployment `server` member,
so Space-inherited read access for other users may stay broken until a joiner
who is not a server member enters the room.

Everything else is lost and is not reconstructed: space and channel owner,
admin, moderator, manager, member, writer and viewer grants; role parents,
assignments and managers; channel `parent` links, except as repaired above;
direct-message participants; message authorship and channel links; and every
channel `outcast` ban in SpiceDB.

Bans are also held in SQL. With clustering enabled (as in production), XEP-0045
admin bans are committed to `clustering_muc_room_affiliations`, and the room
actor denies those outcasts on join itself. They survive the cutover, so a
banned user is not simply let back in. What changes is that SQL and SpiceDB
disagree: the code that reads `outcast` from SpiceDB
(`server/durable_membership.rs`, `server/extension_commands/pubsub.rs`,
`server/extension_host_adapter/implementation.rs`, `admin/channels.rs`
affiliation listing) stops seeing the bans. Re-apply every ban through the
normal XEP-0045 admin path (or the `set-affiliation` ad-hoc command, which
writes the SpiceDB tuple and then commits the same durable affiliation through
the room actor) so both stores agree again. Without clustering there is no SQL
copy, and a banned user can join again until re-banned; keep such rooms closed
(members-only, or not served) until then. Owners of spaces and channels other
than those restored above must be re-granted the same way.

The inventory taken below holds IDs in the previous, ambiguous encoding. Use
it only to decide by hand which rooms to re-ban or re-grant; never replay it
mechanically into the new store. It contains account JIDs: keep it with the
same access controls as the account database and delete it after the window.

## Procedure

1. Before the window, with the previous release still running, inventory what
   will be lost, at least the SpiceDB bans:
   `zed relationship read channel outcast`, and compare with
   `clustering_muc_room_affiliations` where clustering is enabled. Record per-definition counts
   (`server`, `space`, `channel`, `message`, `direct_message`, `role`) so the
   empty store can be verified later. Decide which rooms will be re-banned and
   which kept closed.
2. Stop every replica of the previous release, and every other process that
   writes relationships with the previous encoding, before touching the store.
   Suspend the HelmRelease or scale the Deployment to zero and confirm no pod of
   the previous image remains. **No pre-codec binary may run after the store is
   marked**: it would write legacy IDs into a store this release then reads as
   `v2_` IDs, and the marker cannot detect rows written after it.
3. Empty the store. Either provision a fresh SpiceDB datastore (the planned
   fresh redeploy), or delete every relationship of each definition listed in
   step 1, for example with `zed relationship bulk-delete <definition>` (exact syntax
   and flags to be confirmed against the deployed `zed` version). A single
   bulk delete may stop at a default limit, so repeat it until the verification
   read returns nothing. The existing schema may stay; this release rewrites it
   and adds `waddle_authz_codec` and `waddle_authz_schema`. Verify that `zed
   relationship read <definition>` returns nothing for each definition.
4. Start this release with `Recreate` and the deployment UUID unchanged. One
   replica logs `Marked empty authorization store for the current ID codec`;
   both must become ready. A replica refused because the store `changed while
   its codec marker was being written` only needs a restart.
5. Verify `zed relationship read waddle_authz_codec` returns exactly the
   marker above, and the schema-version checks of
   [authz-schema-versions.md](authz-schema-versions.md) (this release also
   installs schema version 1, which gives a channel `outcast` precedence over
   every channel grant and over the author's own message edit and delete),
   that deployment owners can administer the server, and that a
   fresh account receives its deployment membership. Re-apply the bans (so SpiceDB and
   SQL agree) and ownerships decided in step 1, and confirm the rooms kept closed are closed.
6. Restore `RollingUpdate`, if wanted, only in a separate change after fleet
   verification, under the guard in [relay-cutovers.md](relay-cutovers.md).

## Rollback

Prefer roll-forward. Once the store is marked, the previous release cannot
start against it: its schema write omits `waddle_authz_codec` and
`waddle_authz_schema`, and SpiceDB refuses to remove a definition that still
holds relationships, so its startup fails. That failure is the fence. **Do not restore the previous binary onto a
marked store, and do not delete the marker by hand to get past it**: without
the marker the previous release would start and read every `v2_` row as a
legacy ID, ignoring grants and bans and aliasing identifiers such as
`v2_c2t5`.

A binary rollback is a second cutover in reverse: stop every replica of this
release, empty the store again, including the `waddle_authz_codec` and
`waddle_authz_schema` markers (losing every grant written since, with the same
inventory and re-ban obligations), then start the previous release on the empty
store. Rolling forward again later repeats this whole procedure.

## Refusals

Refusals are logged by each replica as `Refusing the authorization store: it is
not bound to the current ID codec` with the `reason` below, and then fail
startup. The surfaced error depends on where the refusal happens:

- ``authorization store holds `<definition>` relationships but no codec
  marker``: the store was written by the previous release, or a pre-codec
  binary wrote to it before it was marked. Startup fails with
  `Authorization store is not usable with this server: Authorization store
  refused: ...`. Follow the procedure above.
- `authorization store is marked for a different or unrecognised ID codec`:
  the store belongs to another codec version, for example a later release. Run
  the release that wrote it. Do not edit or delete the marker. This is checked
  before the schema is written, so an already-marked store fails startup with
  `Failed to ensure permission schema: Authorization store refused: ...`; the
  same refusal found later, while a replica is marking the store, fails with
  `Authorization store is not usable with this server: ...`.
- `authorization store changed while its codec marker was being written`:
  another writer changed the store during marking. Startup fails with
  `Authorization store is not usable with this server: ...`. Restart; the next
  start decides again.

The local permission backend stores identifiers unencoded in its own tables and
is not affected. It is selected only when SpiceDB is not configured and
`WADDLE_TEST_FIXED_ACCOUNT_ENABLED` is set (a test configuration); otherwise a
missing SpiceDB configuration fails startup. These instructions do not
authorize or assert a deployment, a store deletion or a rollback.
