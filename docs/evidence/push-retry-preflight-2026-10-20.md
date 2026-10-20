# Push retry preflight: 2026-10-20

The checked-in [read-only PostgreSQL inventory](../../server/docs/operations/push-retry-preflight.sql)
completed successfully against the production primary, database `waddle`, at
**2026-10-20 10:32:51.579332 UTC**. The transaction reported `read_only = on` and
used a repeatable-read snapshot. No production writes were performed.

This record contains aggregate counts only. Exact owner, node and job identities
from the restricted operator output are deliberately excluded.

| Provider queue inventory | Count |
| --- | ---: |
| Retained jobs | 1,583 |
| Queued jobs | 0 |
| In-progress jobs | 0 |
| Queued jobs at the 24-failure budget | 0 |
| In-progress jobs at the 24-failure budget | 0 |
| Parked uncertain jobs | 0 |
| Failed known jobs | 82 |
| Rows missing the uncertainty marker | 0 |
| Maximum retained jobs on one node | 762 |
| Nodes near capacity, retaining at least 9,000 jobs | 0 |
| Nodes at capacity, retaining at least 10,000 jobs | 0 |
| Failed known jobs with unsettled outbox lineage | 0 |
| Failed known jobs with unsettled Foundation descendants | 0 |

| Notification outbox inventory | Count |
| --- | ---: |
| Retained jobs | 71 |
| Queued jobs | 0 |
| In-progress jobs | 0 |
| Queued jobs at the five-failure combined budget | 0 |
| In-progress jobs at the five-failure combined budget | 0 |
| Queued or in-progress uncertain jobs at the combined budget | 0 |
| Parked uncertain jobs | 13 |
| Failed known jobs | 1 |
| Rows missing the queue-acceptance marker | 0 |
| Failed known jobs with unsettled outbox lineage | 0 |
| Failed known jobs with unsettled Foundation descendants | 1 |

The combined outbox budget counts ordinary failures and policy errors. Lineage
and Foundation counts describe potentially overlapping sets and must not be
added together.

There were **no active jobs eligible for immediate budget-exhaustion parking**
in this snapshot. No provider node was near its admission cap. The **13 existing
parked uncertain outbox jobs** and **one known-failed outbox job with an
unsettled Foundation descendant** require operator review using the restricted
inventory and the [authenticated administration procedure](../push-retry-policy.md#operator-procedure).
This evidence does not assert that those obligations were retired or that the
known-failed outbox obligation will settle automatically.

The bounded detail queries returned the exact target for the known-failed
outbox obligation under its unsettled Foundation branch. That restricted output
can supply the authenticated inspection and retirement commands; its identities
are excluded from this aggregate record.

These counts describe the recorded snapshot, not the state at deployment time.
Re-run the same inventory before merge and deployment, review any changed
obligations, and retain the timestamp with the new aggregate results.
