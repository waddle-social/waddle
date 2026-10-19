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
- A lost response, transport error or timeout after dispatch remains unknown.
  Local failure before dispatch remains `pre_send`.

These distinctions follow [the APNs response contract](https://developer.apple.com/library/archive/documentation/NetworkingInternet/Conceptual/RemoteNotificationsPG/CommunicatingwithAPNs.html),
[FCM error definitions](https://firebase.google.com/docs/cloud-messaging/error-codes),
and [RFC 8030](https://www.rfc-editor.org/rfc/rfc8030.html). Provider acceptance is
the queue's completion boundary; it is not proof that a device displayed a
notification.

A later refusal never erases uncertainty from an earlier send. Elapsed time,
attempt count and message age do not establish provider acceptance or refusal.

## Scheduling and disposition

The provider queue allows 24 failed scheduling attempts. The notification outbox
allows five failures, including policy-lookup deferrals. Expired claims consume
the relevant budget, so repeated worker crashes cannot restart an unlimited
retry cycle. Retry delays retain bounded backoff and jitter; provider
`Retry-After` delays are capped at one hour.

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
due jobs continue. A known pre-dispatch failure whose parent was contended at
the cap likewise retains its canonical custody until explicit retirement.

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

The existing XEP-0357 registration revocation is the explicit retirement action.
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

Canonical acceptance rows, candidate lineage and required replay evidence remain
protected until authoritative disposition and the configured alias-retention
tail have elapsed. A retry budget must never substitute for that retention rule.
Any rollback must preserve the uncertainty-aware pruning guards: older pruning
code treats failed provider rows as terminal history and cannot safely collect
the new parked representation.

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
