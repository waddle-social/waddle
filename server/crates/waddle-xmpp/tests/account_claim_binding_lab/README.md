# Synthetic account claim and binding laboratory

This PostgreSQL 17 integration fixture is unselected and test-only. It creates
and drops a UUID-named schema containing its own synthetic account claim, one
full-JID slot, a retained set of used binding/install-generation pairs, and an
operation journal. It does not install or inspect a production schema. Setup
creates an owned marker atomically with the schema; setup errors and normal
teardown remove it only after checking the original namespace and marker OIDs,
owners, and random token. A per-name transaction lock fences an uncertain
creation commit before cleanup checks. Tests inject setup failure and a changed
marker to exercise cleanup and refusal respectively.

Every request carries the complete typed `UserClaimIdentity`, `ConnectionRef`,
and `BindingRef`. In one transaction the fixture locks the claim before the
slot, checks the exact identities and version, applies a restricted transition,
and records its complete request and terminal outcome. The schema enforces a
single enrolled slot; new requests for a different slot are rejected, while a
conflicting replay reaches the same journal first. The database's retained
unique binding and `(full_jid, generation)` keys fence reuse. A retry uses the
same operation ID and payload; a mismatch is a conflict. A missing journal row
after an unknown result is indeterminate, including while the original commit
may still be in flight. A replayed historical outcome is not a current bind
authorization; the fixture separately rechecks current claim, binding, and
generation in one database read.

The first test compares a restricted prepare, publish, register, claim-move,
adopt, retire, and fresh-bind history against the in-process #2210 transition
model. The second starts independent test processes, kills one after it has
written but before COMMIT, deliberately withholds a reply after a real COMMIT,
and holds another transaction while PostgreSQL reports the competitor blocked
by that exact child backend. It also recovers a
lost predecessor retirement result after a same-JID successor is active.

The publication, registration, directed-presence, presentation, and custody
receipts are fabricated test inputs. They do not prove an endpoint effect,
actor decision, custody transfer, or teardown. The #2222 child-mutation
observer is diagnostic and cannot supply these missing receipts. The lab does
not cover resource snapshots, live leases, production claim acquisition, actual
UserActor/ConnectionRegistry transitions, physical transport, SM resumption,
or the P2 custody path.

Serving remains **NO-GO**: the active connection path currently publishes its
registry entry before the actor mirror, releases the occupancy guard before
mirror and SM finalization, and actor selection still reads shared entry atomics
and local/remote mirrors. This fixture supplies no authority over those paths.

Run with a dedicated PostgreSQL 17 test database using the repository's normal
`WADDLE_TEST_POSTGRES_URL` configuration:

```sh
cargo test -p waddle-xmpp --features test-utils --test account_claim_binding_lab --locked
```
