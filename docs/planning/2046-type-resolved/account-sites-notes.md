# Account site review shard

Source target: `5763923e0cb15d3bbab9cf53800e5b1bdd41e183`. The
[site ledger](account-site-reviews.tsv) covers the `DUAL`, `UA`, `UAF`,
`UAMSG`, `UR`, `URE`, `URF`, and `URMSG` target families in
[capture-5763923](capture-5763923/README.md): 828 default-profile rows and
904 clustering-profile rows. All 251 scoped manifest tags have coverage rows
in both profiles; 191 have at least one recorded use across the profiles.
The capture is a set of compiler references, not a complete runtime caller or
indirect-edge inventory.

`authority` describes the local decision or state involved at the reviewed
site, not proof of sole durable or distributed ownership. Registration is
both a live mirror and a local bind decision; unregister mirror admission is
not confirmation that the child removed the resource. The custody-aware
local delivery handlers are dark candidates, not classified as obsolete.
`GetAvailableResources` still has the compiler-recorded selection caller at
`server/crates/waddle-xmpp/src/registry/selection.rs:167`, regardless of the
comment about an older retired caller. A constructed or submitted actor
message does not establish successful handling, completed socket delivery,
or live route selection.

In these two captured non-test profiles, no message construction or send was
observed for `TrySendPendingFlush`, `UpdatePresence`, `UpdatePresenceState`,
`GetPresenceState`, `ClearPresenceState`, `GetOtherResources`, `IsConnected`,
`SetCarbonsEnabled`, `IsCarbonsEnabled`, `QueuePendingSubscription`,
`DrainPendingSubscriptions`, or `UnregisterConnection`. Their marked handler
fields and actor state remain locally meaningful when those handlers run; for
example, `TrySendPendingFlush` is submitted by the actor tests at
`server/crates/waddle-xmpp/src/registry/user_actor/tests.rs:1587`.
This observation is limited to the captured markers and checked source; it
does not establish that indirect or other-feature senders are absent, nor that
the handlers are obsolete.

The scoped `validate_site_reviews` check passed against both captured use and
coverage tables, including exact keys, duplicate detection, current source
lines, allowed labels and citation locations. The 52 marker-manifest source
files also matched their retained SHA-256 hashes. The 37 use-site paths include
21 files outside that marker manifest; their recorded source lines were
checked, but this hash comparison does not attest their entire contents. The
full inventory validator, integrated handler and edge ledgers, and runtime
behavior were not checked as part of this shard.
