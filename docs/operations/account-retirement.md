# Retiring an account (#2047)

A Waddle home never issues the JID of a retired account to a new holder
(ADR-0020 D8). Grants follow JID semantics, here and on every deployment
that issued one to the address, so an address that changed holder would hand
the previous holder's grants to the new one. Retirement records the JID in a
ledger that every provisioning path reads
([design](../../server/docs/planning/host-grant-authority-2047.md#non-reuse-of-retired-account-jids-d8)).

Retirement is an operator's act and cannot be undone: no command removes a
JID from the ledger. Use it when an account is deleted for good. To reset a
password or to lock an account out, change its credentials instead.

## What retirement does

One transaction:

- records the account's bare JID, in canonical form, in
  `retired_account_jids`;
- removes the account's native credentials (`native_users`);
- removes its account row (`users`) and, with it, its identity-provider
  links and its sessions.

Then, in the same command, every serving server ends the account's open
sessions. The command records a request; each server takes it up on its
next tick, usually within half a second, and from then on:

- closes every live stream of the account it hosts. The client is sent the
  stream error `<not-authorized/>` (RFC 6120 §4.9.3.12) and the stream is
  closed; the session is not kept for resumption;
- ends every stream-management session of the account that waits for the
  client to resume (XEP-0198), wherever the session's state is held;
- refuses a stream of the account that authenticated before the
  retirement and binds a resource or resumes afterwards, with the same
  `<not-authorized/>`.

Each ended session leaves as a disconnecting client does: its local
contacts receive its unavailable presence, it leaves the local rooms it
occupied (the room tells its occupants, including those on other
deployments), and it leaves the rooms it joined on other deployments.

Each server acknowledges the request with what it ended, and the command
prints one line per server.

Afterwards:

| Path | With the retired JID |
| --- | --- |
| Native registration | Refused. |
| Login through an identity provider whose derived name is the retired one | A new account under the next free name (`name1`, `name2`, …). The same provider subject returning is treated the same way: it gets a new JID, never the retired one. |
| A login whose credentials were checked just before the retirement | No session and no account row is created. |
| A stream that authenticated before the retirement | Its bind or resume ends the stream with `<not-authorized/>`. |
| Fixed-account bootstrap of a test deployment | Fails at startup; configure another name. |

It does not:

- tell the account's contacts on other deployments that it went offline.
  The S2S router refuses everything a retired account sends, its final
  unavailable presence included, so a remote contact may keep showing the
  account's last presence until its own server learns otherwise. Rooms on
  other deployments are left: that leave is the router's own;
- revoke the account's grants in any room, on this deployment or another;
- remove the account's stored data (roster, archive, uploads).

## Procedure

1. **Revoke the account's grants in enrolled communities**, through the
   revocation fence, as in steps 2 to 4 of the
   [reassignment runbook](authz-host-grant-reassignment.md):

   ```sh
   waddle-server host-grant grants --subject user@example.com
   waddle-server host-grant revoke-account --subject user@example.com \
       --operation-id "$(uuidgen)" --account-domain "$WADDLE_XMPP_DOMAIN"
   ```

   An account that is a room's only owner is refused there (`last_owner`):
   have another owner granted first. Revoke its standing in rooms no scope
   enrolls through those rooms' own administration.

2. **Retire the account** while the servers run, so that they end its
   sessions.

   On PostgreSQL, as the migrator login acting as the schema owner, with the
   owner phase's environment (`WADDLE_DATABASE_URL`,
   `WADDLE_DB_MIGRATOR_LOGIN`, `WADDLE_XMPP_MAM_SCHEMA_OWNER`,
   `WADDLE_XMPP_MAM_CLAIM_SCHEMA`):

   ```sh
   waddle-server account retire --jid user@example.com
   ```

   On SQLite, name the server's database file:

   ```sh
   waddle-server account retire --jid user@example.com \
       --sqlite-database /var/lib/waddle/waddle.db
   ```

   The command prints what it did as JSON: the canonical JID, whether this
   run recorded it (`newly_retired`), how many native and account rows it
   removed, and under `sessions` the request it recorded (`request`),
   whether every serving server confirmed (`confirmed`) and one entry per
   server (`replicas`):

   ```json
   {"replica": "<32 hex>", "node": "<node id>/<epoch>", "state": "completed",
    "streams_closed": 1, "detached_sessions_ended": 0}
   ```

   `node` is the server's clustering node identity, or its host name and
   process ID. `state` is:

   - `completed`: the server ended every stream and detached session of the
     account it held, and counted them;
   - `lapsed`: the server stopped renewing its lease before it completed.
     It is stopped or stalled; if it serves again within a day it ends what
     it holds then;
   - `pending`: the server renews its lease and has not completed within
     the wait.

   The command waits at most `--session-wait` seconds (default 30) and
   reports when no server is `pending`; the shortest run takes about a
   second. A server whose lease is not renewed is reported `lapsed` about
   7 s after the command first reads it. With no server running (a stopped
   SQLite deployment, for example) `replicas` is empty: the request stays,
   and a server that starts within a day ends the account's remaining
   detached sessions and refuses its bindings.

   Repeating the command is safe: it reports the JID as already retired,
   removes whatever account rows it still finds and records a new request,
   which every serving server takes up again.

   Exit codes:

   - 0: retired, and no serving server is `pending`;
   - 1: the database returned an error. If the JSON was printed, the account
     is retired and its sessions' end was not requested or not read: run
     the command again;
   - 2: the JID names no account address or the database was not named;
   - 3: retired, and a serving server has not confirmed ending the
     account's sessions (`pending`). Run the command again; a server that
     stays `pending` cannot reach its database or cannot close a stream,
     and its logs say which.

   On PostgreSQL the retirement locks the two account tables for its one
   transaction. Every writer of those tables that arrives while the act
   waits for the lock or holds it waits until the act ends: registrations,
   and also every login, which records a session. The act waits at most 2 s
   for the lock; if a long-running transaction holds the tables it exits
   with code 1 without having changed anything, and logins that queued
   behind it proceed. Run it again. Once it holds the lock, the transaction
   is a handful of statements (five for a usual account) and usually takes
   milliseconds; it has no deadline of its own, and each statement runs at
   most 5 s. The delete of the account row removes the account's
   identity-provider links and sessions with it, and an account with so
   many of them that the delete does not finish is reported with exit code
   1 and nothing changed. The request and the wait that follow lock
   nothing.

3. **Check the report.** The command sees only the servers that hold a
   lease, so first check that `replicas` lists every server that serves
   the deployment. A server is missing when it runs a release without this
   step, when its restricted runtime role lacks the grants the owner phase
   of this release adds, when it cannot write its lease, or when its clock
   runs well behind the command's host (see [Ending sessions](#ending-sessions)); such a server
   ends nothing, and the account's streams on it stay open until the client
   disconnects or the server restarts.

   With every serving server listed, exit code 0 and every server
   `completed` mean that no server holds a session of the account. A
   `lapsed` server that is still running is stalled: once it serves again
   within a day it ends what it holds; restart it if it does not recover. A
   server started more than a day after the retirement holds no session of
   the account from before it started and refuses none of its bindings,
   which the removed credentials and sessions already prevent.

   The step needs every serving server on a release that has it, with the
   owner phase of that release applied first, as for any migration.

4. **Tell the other deployments** that issued grants to the JID, where that
   matters: they cannot see the retirement. A home that keeps this ledger
   never reassigns the address, so their grants stay with nobody rather than
   pass to a new holder.

## The ledger

`retired_account_jids` holds one row per retired JID: the canonical bare JID,
its canonical localpart and when it was retired. Rows are never updated or
deleted. Under the restricted runtime role the server can read the ledger and
nothing else, so a server can neither retire a JID nor make one available
again. Keep the ledger in every backup and restore it with the account
tables: a database restored to before a retirement holds the account again
and no ledger row, and the retirement has to be repeated.

## Ending sessions

Three tables, on both engines (migration 2015), carry the second step:

- `account_session_ending_replicas`: one lease per serving server process,
  registered when it starts serving, renewed every half second for 5 s, and
  ended when it stops;
- `account_session_ending_requests`: one row per run of the command, naming
  the retired JID;
- `account_session_ending_acks`: each server's progress on a request: when
  it took the request up, when it completed it, and what it ended.

A server acts on a request of the last day whose JID the ledger holds.
Under the restricted runtime role a server registers and renews its lease
and records its acknowledgements; it reads requests and deletes nothing.
The command removes leases that ended more than a day ago and requests
older than two days, with their acknowledgements, each time it runs.

The command decides that a server serves by seeing its lease renewed while
it waits, on its own clock; which leases it looks at depends on the clocks
of the hosts, and it looks at every lease whose recorded end is no earlier
than 10 s before the request. Keep the hosts' clocks in sync (NTP): the
command does not wait for a server whose clock runs more than about 14 s
behind its own.
