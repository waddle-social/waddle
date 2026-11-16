# After a reported reassignment of a remote account JID (#2047)

Grants follow JID semantics. A grant issued to `user@remote.example` belongs
to whoever holds that address at its home, and the home decides who that is:
it can re-register a deleted account, and its domain can change hands.
Waddle does not detect either (ADR-0020 D8). When someone reports that a JID
this deployment issued grants to now belongs to another holder, an operator
revokes the JID's grants and issues any grant for the new holder only after
confirming the holder with the remote home's operator, outside the protocol.

This runbook is that procedure for communities enrolled under the host-grant
authority ([design](../../server/docs/planning/host-grant-authority-2047.md#revocation-fence-decisions)).
It applies equally when the report concerns an account of this deployment's
own domain; there the JID should also be retired
([account retirement](account-retirement.md)), which a Waddle home enforces
durably.

The acknowledgement this procedure waits for holds only under the fence's
clock assumptions: the servers run on Linux, and the PostgreSQL hosts, a
standby that a failover would promote included, are kept in sync by NTP
with an offset well under 1 s. A database clock that runs ahead ends the
servers' leases early, and a revoke can then be acknowledged while a server
still acts on a decision it read before. Before relying on an
acknowledgement, check that no server reports
`waddle.host_grant.fence.clock_skew` above 500 ms and that its log shows no
`clock_skew` warning ([metrics](server-metrics.md)).

## What the report changes

- **A home that is not on the trusted-home list.** The previous holder's
  member-level grants now admit the new holder. That is the accepted
  limitation D8 records: such an account never holds room moderation or a
  privileged grant, and its member-level access lasts until the revoke below.
- **A trusted home.** A trusted home is trusted for non-reuse. If it
  reassigned the JID anyway, the new holder also holds the previous holder's
  owner or admin standing and room moderation on `pkix` streams until the
  revoke below. Decide whether the home stays on the trusted-home list at
  all: removing it withdraws privileged authority and room moderation from
  every account of that home at once, without revoking grants one by one.
- **A domain that changed hands.** Every JID of the domain is affected, not
  one. Remove the domain from the trusted-home list and from the allowlist,
  or add it to the denylist, before anything else; then revoke as below for
  each JID that holds a grant.

## Procedure

The commands are operator subcommands of the server binary. They run as the
migrator login acting as the schema owner, with the owner phase's
environment (`WADDLE_DATABASE_URL`, `WADDLE_DB_MIGRATOR_LOGIN`,
`WADDLE_XMPP_MAM_SCHEMA_OWNER`, `WADDLE_XMPP_MAM_CLAIM_SCHEMA`), and can run
while the servers run. With `WADDLE_SPICEDB_ENDPOINT` and
`WADDLE_SPICEDB_PRESHARED_KEY` in the environment they also write the act's
restore witnesses at once
([restore runbook](authz-host-grant-restore.md)).

1. **Record the report.** Who reported it, when, which JID, and what is
   known about when the address changed holder. The time matters for step 5.

2. **List what the JID holds.**

   ```sh
   waddle-server host-grant grants --subject user@remote.example
   ```

   One JSON line per room: the scope's lifecycle, the room, the recorded and
   the effective affiliation. A row whose affiliation is `none` is an earlier
   revoke. Keep the output: it is the record of what the previous holder
   held, and step 6 starts from it.

3. **Revoke everywhere, through the fence.** Generate one operation ID and
   keep it with the report.

   ```sh
   waddle-server host-grant revoke-account \
       --subject user@remote.example \
       --operation-id "$(uuidgen)" \
       --account-domain "$WADDLE_XMPP_DOMAIN"
   ```

   The command records a revoke in every managed room where the JID holds a
   grant, and for each one waits until the revocation fence has acknowledged
   it. It prints one JSON line for every room the JID holds a row in. A room
   where the JID already holds what is asked for (an earlier run revoked it,
   or it was revoked another way) records nothing and is reported with
   `"unchanged": true` and the fence's answer at its scope's current
   generation, which covers that earlier revoke: a repeated run confirms
   every room, not only those it changed. Each revoke is effective at its commit: from then on no new use of the
   room is admitted under the grant. The acknowledgement says more: every
   server that may still decide for the scope has installed the revoke, so
   none acts on a decision it read before. Each server then removes the
   account's occupants from the rooms it hosts (status 321) and ends their
   media sessions.

   Add `--ban` to record a ban instead: the JID then cannot be granted
   membership again without lifting the ban first, which is the safer state
   while the new holder is unconfirmed.

   Read the exit code:

   | Exit | Meaning | Do |
   | --- | --- | --- |
   | `0` | Every room's revoke is recorded and acknowledged. | Go on. |
   | `1` with `"error"` on a line | That room's revoke, or the reading of its acknowledgement, failed; the other rooms are reported on their own lines. | Run the same command again with the same operation ID. |
   | `1` with `"acknowledged": false` on a line | That revoke is recorded and effective; a server that holds a lease has not installed it yet. A server that stopped is waited for until its lease has run out (about 6 s). | Run the same command again with the same operation ID. It replays the recorded revokes, reports the rooms revoked before, and waits again. If a server stays behind, look at that server: `host-grant fence --scope <lifecycle>` shows the scope's standing. |
   | `1` otherwise | The database returned an error before any room was reported. | Run the same command again with the same operation ID. |
   | `4` with `"refusal": "last_owner"` on a line | The JID is that room's only owner of a domain this deployment serves; the other rooms are revoked. | This cannot be a remote JID. For a local account, have another owner granted through the room's administration, then repeat the command. |
   | `4` otherwise | The store refused the act. | Read the diagnostic. |

   A line with `"refusal": "scope_retired"` or `"scope_exhausted"` names a
   scope that is closed for good: nothing is admitted there, nothing is
   recorded, and the exit code is not affected.

4. **Confirm.** `host-grant grants --subject …` now shows `none` (or
   `outcast`) for every room. A scope that was closed for a restore keeps the
   revoke; it applies when the scope is reopened.

5. **Review what the new holder could do before the revoke.** Between the
   reassignment and step 3 the new holder was admitted as the previous one.
   For each room in step 2's output, review the archive and the
   administration log for that period: messages sent, and, for a trusted
   home's privileged grant, affiliation changes and moderation. Revert what
   should not stand through the room's administration. The server keeps no
   record that distinguishes the two holders.

6. **Issue a grant for the new holder only after confirmation.** Confirm the
   holder with the remote home's operator through a channel outside XMPP.
   A message from the JID itself confirms nothing: the new holder controls
   it. Once confirmed, a room owner grants the affiliation through the
   room's administration (XEP-0045), as for any new member; lift a ban first
   if step 3 recorded one. A privileged or moderator affiliation is issued
   only to an account of a trusted home.

## What this procedure does not cover

- **Rooms no scope enrolls.** Their grants are the legacy channel and Space
  relationships and PubSub affiliations. Revoke the JID there through the
  room's and the Space's own administration; those changes take effect at
  their commit and have no acknowledgement across servers.
- **Other deployments.** A grant another deployment issued to the JID is
  that deployment's to revoke.
- **Detection.** Nothing here notices a reassignment. A home that reuses
  addresses routinely should not hold grants a new holder must not inherit;
  for a trusted home that is a reason to remove it from the list.
