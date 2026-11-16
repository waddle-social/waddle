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

Afterwards:

| Path | With the retired JID |
| --- | --- |
| Native registration | Refused. |
| Login through an identity provider whose derived name is the retired one | A new account under the next free name (`name1`, `name2`, …). The same provider subject returning is treated the same way: it gets a new JID, never the retired one. |
| A login whose credentials were checked just before the retirement | No session and no account row is created. |
| Fixed-account bootstrap of a test deployment | Fails at startup; configure another name. |

It does not:

- end the account's open streams: a connected client stays connected until
  it disconnects or the server restarts, and cannot resume or log in again;
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

2. **Retire the account.**

   On PostgreSQL, as the migrator login acting as the schema owner, with the
   owner phase's environment (`WADDLE_DATABASE_URL`,
   `WADDLE_DB_MIGRATOR_LOGIN`, `WADDLE_XMPP_MAM_SCHEMA_OWNER`,
   `WADDLE_XMPP_MAM_CLAIM_SCHEMA`):

   ```sh
   waddle-server account retire --jid user@example.com
   ```

   On SQLite, name the server's database file. The server may keep running;
   the act is one short write transaction.

   ```sh
   waddle-server account retire --jid user@example.com \
       --sqlite-database /var/lib/waddle/waddle.db
   ```

   The command prints what it did as JSON: the canonical JID, whether this
   run recorded it (`newly_retired`), and how many native and account rows it
   removed. Repeating it is safe. Exit code 2 means the JID names no account
   address or the database was not named; 1 that the database returned an
   error, and the command can be run again.

   The servers can run during the act. On PostgreSQL it locks the two account
   tables for its one transaction. Every writer of those tables that arrives
   while the act waits for the lock or holds it waits until the act ends:
   registrations, and also every login, which records a session. The act
   waits at most 2 s for the lock; if a long-running transaction holds the
   tables it exits with code 1 without having changed anything, and logins
   that queued behind it proceed. Run it again. Once it holds the lock, the
   transaction is a handful of statements (five for a usual account) and
   usually takes milliseconds; it has no deadline of its own, and each
   statement runs at most 5 s. The delete of the account row removes the
   account's identity-provider links and sessions with it, and an account
   with so many of them that the delete does not finish is reported with
   exit code 1 and nothing changed.

3. **End the account's open streams** if it must stop at once: restart the
   servers, or wait for the client to disconnect. A retired account cannot
   establish a new session.

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
