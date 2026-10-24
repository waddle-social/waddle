# Remote-dispatch completion backlog: issue #2018

Read-only investigation on 2026-10-24, 22:48–23:08 UTC. No production rows,
receipts, ownership claims, configuration or application processes were changed.
This record deliberately omits message contents and user, room, node and message
identifiers.

## Findings

The primary-only Grafana series reproduced the reported increase from one to two
aged canonical messages at **20:01:30 UTC**. The primary sentinel was about 30
seconds fresh at the follow-up observation, and the count remained two. The
10-minute monitoring threshold explains why the new row first appeared then:
its intake timestamp is **19:51:07.916412 UTC**.

Repeatable-read, read-only transactions against `postgresql-1`, with the
application role `waddle`, found these two obligations:

| Intake timestamp (UTC) | Retained protocol shape | Completion evidence |
| --- | --- | --- |
| October 17, 13:46:06.908414 | Body-bearing groupchat, origin ID and markable extension | One kind-12 dispatch intent; no matching effect receipt, delivery receipts or descendants; one origin alias |
| October 24, 19:51:07.916412 | Bodyless groupchat carrying only XEP-0085 `composing` | One kind-12 dispatch intent; no matching effect receipt, delivery receipts, descendants or origin alias |

Both retained rows use envelope V1 and intent payload V1. Their original
destinations are different historical node incarnations, both now recorded as
expired. The current room claim belongs to another, non-expired node. This
establishes present ownership, not ownership or reachability at event time.

An exact room/archive-origin-ID lookup for the older row, plus room-scoped
message/stanza-ID comparisons, found no matching MAM row. The newer row has no
origin alias and is a transient composing notification, for which absence from
MAM does not establish a fault. Neither result proves recipient delivery or
message loss: the retained dispatch intent contains no authoritative owner
acceptance or frozen recipient manifest, and archive lookup is not a substitute
for that evidence.

## Cause established and limits

The newly counted item is **a composing notification**, not a newly identified
lost body-bearing message. Both rows are retained because their exact remote
dispatch completion receipts are missing. The current maintenance executor
explicitly cannot reconstruct kind-12 dispatch from the recorded envelope and
intent alone. Repeated partial maintenance evaluations therefore cannot clear
these obligations and are not additional affected messages.

The retained state does not distinguish an unperformed dispatch from a dispatch
whose completion proof was lost. This investigation does not establish the
event-time failure mechanism or attribute it to PR #2017: intake of the newer
row predates that PR's merge and process replacement. The October 17 body-bearing
row still requires authoritative owner-side evidence before delivery can be
classified.

## Supported next action

Use the bounded [read-only shape diagnostic](../../server/docs/operations/remote-dispatch-backlog.sql)
on the primary application database. It reports at most 100 aged dispatch pairs
without exposing payload text or identifiers. For exact operator correlation,
use the private pending-pair manifest procedure in the
[ingress authority runbook](../../server/docs/operations/ingress-authority.md#non-terminal-backlog-triage).
Do not insert synthetic receipts, retarget an old dispatch to the current owner,
or delete a canonical row solely because a destination expired.

Durable owner acceptance and restart reconciliation belong to
[#1661](https://github.com/waddle-social/waddle/issues/1661),
[#1986](https://github.com/waddle-social/waddle/issues/1986) and
[#1948](https://github.com/waddle-social/waddle/issues/1948). They must preserve
the original manifest and responsibility across owner changes. An explicit
expiry/disposition policy for transient chat states would need a separate
reviewed change; this investigation does not invent one.

Validation: the diagnostic was executed as one read-only transaction on the
primary; independent shape, pending-pair, node/claim and archive-ID queries
agreed with the classification above. No live repair was attempted.
