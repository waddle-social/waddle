# Stream management quota recovery verification

Follow-up tracking: [#1971](https://github.com/waddle-social/waddle/issues/1971),
[#1972](https://github.com/waddle-social/waddle/issues/1972), and
[#1427](https://github.com/waddle-social/waddle/issues/1427).

PR #1970 adds durable promotion custody and quota recovery instrumentation.
Operational acceptance requires deployment and eligible traffic; passing local
tests alone does not establish recovery of historical queues.

## Verification plan

1. Establish the server deployment revision and rollout interval. Compare equal
   windows before and after rollout for missing-target warnings, quota rejection
   increments, and pending-delivery age-out increments.
2. Classify historical senderless rows using aggregate metadata and XML envelope
   shapes from [the producer audit](../sm-promotion-producer-audit.md). Never
   publish message contents, recipient addresses, or account credentials.
3. With designated test accounts, fill a recipient queue while its local sender
   is offline. Reconnect the sender and verify exactly one `service-unavailable`
   error, including after another reconnect.
4. Record configured retention, eligible row counts, observation windows, and
   counter deltas. Verify expired rows free recipient quota. Zero age-outs with
   no eligible rows is inconclusive.
5. Record evidence and remaining access gaps on #1427. Propose any historical
   cleanup separately, beginning with aggregate dry-run counts.

The #1972 implementation must keep an expired resume's recorded queue in durable
non-resumable inventory before releasing custody. Regression coverage will
exercise expiry during completion, restart before promotion, exact fencing,
cancellation, successful resume, and portable receipt reclamation on SQLite and
PostgreSQL.
