# Push retry budgets and retained custody

The notification outbox and provider queue limit automatic retries. Exhausting a
retry budget stops scheduling; it does not prove that a provider accepted or
refused a notification. Custody of an unresolved accepted notification can
therefore outlive its retry budget.

## Failure certainty

Each failed attempt carries a typed failure class independently of its retry
reason and scheduling disposition:

| Class | Evidence from this attempt | Effect on earlier uncertainty |
| --- | --- | --- |
| `pre_send` | Dispatch did not reach the notification provider, such as a local preparation or OAuth-token failure. | Retained. |
| `proven_refusal` | A documented provider response establishes that this request was refused. | Retained. |
| `unknown_after_send` | A started request has no authoritative acceptance or refusal result. | Set and retained. |

Throttling is a retry reason with a refusal result. Provider server errors require
provider-specific classification:

- APNs HTTP 500 `InternalServerError` and HTTP 503 `ServiceUnavailable` or `Shutdown`
  are documented failed requests and count as refusals. Other server-error
  responses without that documented meaning remain unknown.
- FCM HTTP 503 with the exact `UNAVAILABLE` result establishes inability to process
  that request. FCM HTTP 500 `INTERNAL` and unrecognized server-error results
  remain unknown.
- Arbitrary Web Push HTTP 5xx responses remain unknown. An HTTP status family alone
  does not establish whether a push service accepted a message.
- A request whose connection fails before it can send is `pre_send`; the HTTP
  transport uses the typed connect-error classification for that boundary.
  A timeout after connection, lost response, or other transport failure that
  cannot prove no request was sent remains `unknown_after_send`.

Web Push, APNs, FCM and OAuth token HTTP clients use a five-second connection
setup timeout inside their ten-second overall request timeout. A stalled
connection therefore retains the transport's pre-send evidence.

These distinctions follow [the APNs response contract](https://developer.apple.com/library/archive/documentation/NetworkingInternet/Conceptual/RemoteNotificationsPG/CommunicatingwithAPNs.html),
[FCM error definitions](https://firebase.google.com/docs/cloud-messaging/error-codes),
and [RFC 8030](https://www.rfc-editor.org/rfc/rfc8030.html). Provider acceptance is
the queue's completion boundary; it is not proof that a device displayed a
notification.

A later refusal never erases uncertainty from an earlier send. Elapsed time,
attempt count and message age do not establish provider acceptance or refusal.

## Scheduling and disposition

The provider queue allows 24 failed scheduling attempts. Its retry delay grows
exponentially from 60 seconds, doubling with the persisted failure count and
capping at one hour after jitter. The same persisted count drives ordinary
provider failures, pre-send repair and stale-claim recovery. Provider
`Retry-After` can increase the delay, within the one-hour cap.

The notification outbox shares a five-failure budget across ordinary failures,
abandoned stale claims and policy-lookup deferrals. Known-unaccepted ordinary
failures use exponential delays of 5, 10, 20 and 40 seconds before the final
attempt, with jitter. Once queue acceptance may exist, delays become 5, 10, 20
and 40 minutes, with jitter and a one-hour cap. Five attempts therefore span
roughly an hour or more, allowing ordinary provider recovery before parking.
Expired claims consume the relevant budget, so repeated worker crashes cannot
restart an unlimited retry cycle. Existing uncertainty survives every retry.

Fresh work whose failures prove nonacceptance can become a terminal known
failure when its budget expires. Its unowned canonical lineage can settle.
Unresolved accepted work with possible acceptance instead parks when its budget
expires. Parking clears its claim and retry deadline, retains its frozen payload
and acceptance evidence, and does not settle its outstanding canonical obligation.

The existing storage states distinguish parking from terminal failure:

| Queue | Parked representation | Terminal known failure |
| --- | --- | --- |
| Provider | `status = 'failed'` and `uncertain_send = 1`, without an authoritative retirement disposition. | Failed with no earlier uncertainty. |
| Notification outbox | `status = 'failed'` and `queue_acceptance_may_exist = 1`. | Failed with known nonacceptance. |

Parked work does not resume automatically, including after restart, device
registration, or an identical enqueue replay. There is no automatic replay or
operator resume operation. An identical canonical enqueue returns the existing
acceptance without creating a new obligation.

Parked outbox owners still receive bounded, rotating checks for authoritative
provider completion or revocation. Their otherwise idle `next_attempt_at_ms`
stores the last proof-probe timestamp; failed status excludes them from sending.
An unavailable proof or contended parent leaves that owner retained while other
due jobs continue. Provider failures with proven nonacceptance also receive a
bounded settlement sweep after fresh delivery work. A contended parent or child
row defers settlement without resending or changing the failure history. Known
failed outbox owners with unsettled custody remain available for explicit
retirement.

## Capacity and operator action

The hard provider admission limit is 10,000 retained jobs per node. Parked jobs
and terminal evidence within its retention tail both count. Admission prunes
only eligible terminal history; it never discards an unresolved accepted job to
make room. A fresh key at capacity receives XMPP `resource-constraint` with error
type `wait` before acceptance. Other nodes retain their own admission capacity
and continue progressing.

This policy bounds retry activity and retained provider jobs per node. It does
not promise a finite custody lifetime for unresolved accepted work. A saturated
node needs authoritative disposition to free its retained obligations, followed
by expiry of the retention tail. Moving a job to parked storage does not itself
create admission headroom.

The existing atomic XEP-0357 registration revocation is the retirement action.
It cancels queued, in-progress and parked work for that registration, fences old
claims, records `registration-revoked`, and settles the corresponding canonical
custody. It does not claim that an earlier uncertain send failed or prevent a
provider from completing a request already in flight. Terminal-only outbox
reconciliation consumes the exact provider acceptance's completion or revocation
proof and acknowledges completion upstream without resending parked work.
Explicit retirement also closes matching host outbox owners in the same
transaction when a crash occurred before a provider row was created. Repeating
the explicit disable can retire orphan owners even if the registration is
already absent; ordinary registration absence or automatic disabling is not
authority to release uncertain custody. Repeating a completed retirement does
not restart its retention tail.

Retirement also disables the matching enabled host push registration, preventing
new notification candidates from repeatedly targeting the retired endpoint. It
preserves unrelated registrations and already settled failure history. If any
canonical parent lock contends, the transaction rolls back and the command
returns an error: retry the complete retirement after contention clears. There
is no partial-success disposition.

Host registrations and outbox owners are retired only for the configured push
service. An identical owner/node pair on another service is outside that
authority. A standalone provider store without a configured service can retire
its own provider state but leaves service-keyed host records untouched.

Canonical acceptance rows, candidate lineage and required replay evidence remain
protected until authoritative disposition and the configured alias-retention
tail have elapsed. A retry budget must never substitute for that retention rule.
Any rollback must preserve the uncertainty-aware pruning guards: older pruning
code treats failed provider rows as terminal history and cannot safely collect
the new parked representation.

## Operator procedure

Before deploying the retry policy, run the checked-in
[read-only preflight inventory](../server/docs/operations/push-retry-preflight.sql)
against the authoritative PostgreSQL database using `psql -X -v ON_ERROR_STOP=1
-f server/docs/operations/push-retry-preflight.sql`. The script discovers tables
and columns, uses a repeatable-read snapshot and local query timeouts, and makes
no changes. Record its `snapshot_at` with the aggregate counts. Detail lists are
limited to the oldest 200 matching jobs per query and the fullest 100 nodes;
aggregate counts cover all matching database rows. Separate known-failed detail
queries return exact owner/node/job identities for unsettled lineage and
Foundation custody, including work with known nonacceptance. Use those identities
with the authenticated commands below when `PushUnresolvedFailedCustody` fires.
Lineage and Foundation results may overlap; deduplicate job IDs rather than
adding those counts. Logs establish what occurred, while the database inventory
establishes what remains retained now.

Review queued and in-progress provider work already at 24 failures, uncertain
outbox work at five combined failures, parked uncertain work, failed known work
with unsettled custody, and node admission headroom. Preexisting work at its
budget can park under the bounded policy; inventory it before the rollout and
choose explicit retirement where appropriate. Retain the snapshot as restricted
operator evidence. It includes exact owner/node/job identities but excludes
message payloads, provider credentials and raw diagnostic errors.

Use an authenticated community-owner administrator account for the native
XEP-0050 commands:

1. Execute `urn:waddle:admin:push:inspect:0` with no form to obtain the data form.
   Submit `FORM_TYPE` equal to that command node, `owner` as the exact local bare
   user JID, and `node` as one exact nonempty push node. Review the inventory for
   that pair and verify which registration will be retired.
2. Execute `urn:waddle:admin:push:retire:0` and submit the same exact `owner` and
   `node`, `FORM_TYPE` equal to the retirement command node, and boolean `confirm`
   set to `true` or `1`. For the form flow, complete the returned command session
   using its session ID. Missing or false confirmation makes no change. A
   one-shot execute carrying the complete submitted form is also supported.
3. After success, inspect the same pair and repeat the read-only database
   inventory. Verify the registration is disabled and unresolved custody has
   settled; retained terminal history can remain until its tail expires. If
   retirement reports lock contention, retry the complete command. Do not infer
   success from a request timeout; inspect and, when needed, repeat the exact
   confirmed retirement, which preserves an already completed retention tail.

The commands require administrator authorization; no owner password is needed.
Do not use raw SQL deletes or clear uncertainty flags to create headroom. There
is intentionally no resume command. Re-enabling a registration is a separate
registration decision and does not replay the retired or parked backlog.

For this rollout, refresh the production preflight **immediately before merge**
and again before deployment. The recorded inventory identifies 13 parked
uncertain outbox obligations and one known-failed unsettled outbox obligation.
The rollout plan includes inspection and authoritative retirement of these
**14 obligations** after the administrator commands are deployed. Obtain the
exact owner/node targets from the refreshed restricted inventory, group jobs by
that pair, inspect each pair, and explicitly confirm its node-wide retirement
using the procedure above. A count does not itself authorize an automatic
retirement; changed targets or additional work require inspection before that
confirmation. No production retirement has been performed by the preflight.

Afterward, verify settlement and disabled registrations with the read-only
inventory. In Grafana/Mimir, inspect the actual deployed exported metric names
and units, confirm the recording rules match those series, and compare fresh
parked/unresolved gauges with the database result. Verify parked-work, unsettled
custody, capacity and snapshot-freshness alerts evaluate correctly and recover
when the corresponding condition clears. Retained terminal rows can continue
to consume capacity during the retention tail.

## Diagnosis

Structured push transition logs include the bounded retry reason, failure class
and disposition, together with the diagnostic error. Raw diagnostic text is not
a metric attribute. `retry_scheduled` and `dead_lettered` transitions retain their
specific cause instead of assigning every failure `unknown`.

The metrics `xmpp.push.failure_reason`, `xmpp.push.failure_class` and
`xmpp.push.disposition` export these allowlisted dimensions separately to bound
cardinality. Use `reason = capacity` with `disposition = backpressure` to detect
admission pressure, and `disposition = parked` with unknown-after-send failure
evidence to identify work requiring disposition. Logs retain event-level
correlation; separate metric series do not establish that two counts refer to
the same notification. Existing outbox retry and dead-letter counters continue
to report their corresponding scheduling transitions.

Current-state gauges supplement transition counters:

| Metric | Meaning |
| --- | --- |
| `xmpp.push.queue.parked{queue=outbox\|provider}` | Current uncertain parked rows without an authoritative retirement disposition. Provider work still counts after upstream ownership has completed. |
| `xmpp.push.queue.unresolved_failed{queue=outbox\|provider}` | Current known-failed rows with actual unsettled lineage or Foundation descendants. |
| `xmpp.push.queue.snapshot_at{queue=outbox\|provider}` | Unix seconds of the last successful database inventory for that queue. |
| `xmpp.push.provider.retained_max_per_node` | Largest current retained provider-job count on one node, including terminal tails. |
| `xmpp.push.provider.near_capacity_nodes` | Number of nodes retaining at least 9,000 jobs, against the 10,000 admission cap. |

Count gauges use annotation units `{job}` and `{node}` so their Prometheus
names remain unsuffixed, such as `xmpp_push_queue_parked`. The snapshot gauge
uses `s` and translates to `xmpp_push_queue_snapshot_at_seconds`. The shared
exported-unit fixture and real OTLP ingestion test verify these names against
the actual recording rules; a dimensionless gauge unit `1` would add `_ratio`.

Replicas sharing the database report the same inventory. First exclude each
replica's values when its own queue snapshot is older than five minutes, then
aggregate fresh values with `max`, never `sum`. This prevents an old high value
from overriding another replica's fresh zero. Missing or stale snapshots and
failed inventory reads mean current capacity is unknown; old or absent samples
must not be read as zero. The snapshot freshness alert waits five minutes after
the five-minute threshold. Exact identities remain in the restricted read-only
inventory and structured logs, outside metric attributes.
