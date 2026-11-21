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

### Authoritative call dispositions

Call invitations have a fixed deadline: the original message receipt time in
Unix seconds plus 45 seconds. Before provider dispatch, a call past that deadline
on its VoIP node ends `failed` with `uncertain_send = 0`, an error identifying call expiry, and
settled notification ancestry. This is an explicit authoritative expiry
disposition, including when an earlier attempt left `uncertain_send = 1`; the
transport policy TTL does not extend the deadline.

An archived bodyless XEP-0353 audio/video `<propose/>` (requiring the existing
XEP-0334 `<store/>` hint) creates one `call` candidate per recipient account,
including accounts with connected or hibernated
(XEP-0198 detached) sessions and proposals addressed to a detached full JID.
The selected recipient pass prepares this candidate once before resource
fanout. Live/detached delivery creates no XEP-0160 pending row; headless
delivery retains its existing pending-row path. Like the headless path, the
live path requires a recipient archive write with the candidate's stanza id;
a recipient `<stanza-id/>` alone does not prove archival. Both paths persist
the reason `offline_call`; that name does not imply the account has no sessions.
Blocking, self-notification rejection and the existing T0/T1 push gates apply.

Ringing pushes allow at most 3 `call` notification candidates per caller bare JID
and recipient account in 60 seconds, and 10 in an hour. This is a soft limit
across concurrent sessions/replicas. All existing `call` rows count, including
later-suppressed, outboxed and quarantined rows. This affects push only: invitations
remain routed, archived, delivered to connected devices and retained for offline
delivery. A rate-limited invitation has no candidate and yields no missed-call
alert. A resent invitation (same origin-id) is planned under a
fresh archive id, so it counts its own first candidate and may be suppressed;
that first candidate and its push stand. Suppression emits the metric-only `waddle_call_rate_limit` reason; no
migration, schema change or new stored suppression value is needed.

The live candidate and its ingress receipt commit together with canonical
ancestry. Replays use the recorded archive stanza id and original receipt time.
If that effect was lost before execution, maintenance settles its recorded
candidate obligation without inserting a late push: its 60-second grace exceeds
the call's 45-second lifetime. A T0 preparation failure logs a warning and
increments `xmpp.push.live_call_preparation_failed`; no pending row exists to
anchor a retry.

During a rolling update, an older binary cannot settle a DM candidate intent
without a pending row and may report it as an unrecoverable obligation. A newer
binary's maintenance settles that expired obligation. This invitation-recovery
path needs no persisted encoding change; the missed-call class described below
requires migration 2002. After a full rollback to a binary without live candidate
recovery, ingress rows with recorded but unexecuted live candidate obligations
stay non-terminal and raise the non-terminal backlog alert.

A newly inserted `call` candidate wakes this process's notification outbox loop
only after its durable ingress transaction commits. A wake drains only call
candidates and due call outbox jobs. Other classes, candidate recovery and
pruning stay on the outbox timer. Confirmed queue acceptance of a call job wakes
this process's Push Service job loop, which drains due jobs of all classes.
Calls therefore avoid waiting for the default five-second outbox and ten-second
provider timers. The `Notify` permits survive a busy loop and coalesce repeated
wakes. These are local scheduling hints, with no cross-replica signalling:
another replica can claim the work first, and timers remain the fallback for
recovery, retries and ambiguous queue acceptance. Call work triggers both wakes.

An archived plain XEP-0353 `<retract/>` creates class `missed_call`, reason
`call_retracted`, only when an unquarantined `call` candidate for the same
recipient, caller bare JID and SID was created within the preceding 60 seconds.
The comparison uses the retract's original server receipt time, including
recovery; already outboxed or suppressed invitations qualify. Another caller
resource may cancel. A partial unique index permits one missed-call candidate
per recipient, conversation and SID, even for repeated distinct retracts.
A call that rings out creates the same class with reason `call_unanswered`:
the timed outbox sweep (both replicas) finds `call` candidates created more
than 55 seconds ago with no missed-call row and no outcome in the callee's own
archive, and inserts one candidate without ingress lineage. An outcome is any
archived `proceed`, `accept`, `reject`, `finish` or `retract` for the SID with
the caller, tie-break included. The deadline is 55 seconds so the caller's own
45-second retract wins. Every sweep covers the last 5 minutes of invitations,
including ones written late by recovery; calls that rang out during a longer
outage get no alert.

Missed calls carry the SID without video and no `<call/>` context, so they use
ordinary APNs alert nodes and Web Push, never VoIP nodes. APNs uses
`WADDLE_PUSH_MISSED_CALL`; Web Push uses `class: "missed_call"`, ordinary DM
TTL/urgency and a separate topic. FCM devices are not applicable until Android
supports the class. The published summary carries the conversation's real
unread count, including zero; APNs reads the account-wide badge immediately
before dispatch. Zero unread does not suppress the alert. Missed calls
remain on the ordinary timer drain and do not trigger the call wake path.
Mute at T0, DND/blocking at T1, and blocking at publish apply as for call invitations.
An unexecuted live missed-call receipt expires after the same 60-second
maintenance grace as a live invitation receipt.

Other bodyless JMI signals (`proceed`, `reject`, `finish`, `ringing`, legacy
`accept`, and ineligible or tie-break retracts) create no notification candidate
or candidate activity preview. Their XEP-0160 pending delivery,
`OfflineDelivery` marker and MAM storage remain intact. Messages with a body,
with a proposal, or with only unknown JMI children follow normal notification
policy.

A retract is suppressed when the callee's archive already holds its own
`proceed`, `accept`, `reject` or `finish` for the SID. An answer and a retract
committed at the same instant may still both miss each other; a retract planned
before the proposal candidate commits may miss eligibility. A JMI reply sent
without the `<store/>` hint is not archived and cannot suppress an alert;
Waddle's clients always add it. Deploy only after
Apple adds the loc-key and conversation tap route and the web service worker
adds its “Missed call” title. Migration 2002 requires the owner Job at zero
replicas using `server/docs/runbooks/restricted-runtime-cutover.md`.

Call jobs apply only to the node whose app ID exactly matches the configured
APNs bundle ID plus `.voip`; other notification classes apply only to other
nodes. A class/node mismatch ends `published`, clears `uncertain_send`, and
settles ancestry without sending. The diagnostic records `not-applicable`,
which acknowledges upstream custody without claiming provider delivery. Class
and node app ID are immutable, so an earlier pass cannot have sent such a job.
On the VoIP node, a non-APNs device records a non-retrying, non-disabling
not-applicable attempt. Applicability is decided before expiry, and both run
before empty-device retries.
The configured topic identity is retained independently of provider readiness.
When APNs configuration is entirely absent, there is no known VoIP topic:
call jobs are not applicable, and an app ID ending in `.voip` alone does not
establish a matching node. Existing non-call provider-not-configured handling
continues to apply in that case.

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

### Legacy registrations and disabled-node retention

Provider node names are opaque. Older registrations can name a local service
without a provisioned provider node; a missing node does not establish whether
it was deleted or never created. Foreign-service registrations intentionally
have no foreign key to local `push_nodes`.

Automatic disabled-node pruning retains 64 recent disabled nodes per owner,
plus nodes protected by delivery custody or an enabled registration at the
configured local service. The latter prevents pruning from creating a new
enabled local orphan. Foreign-service registrations do not pin local provider
nodes, and a standalone store cannot infer which host service is local.

Pruning is retention-only: it does not cancel provider jobs, clear active
claims, rewrite frozen payloads, assign a retirement disposition, or settle
outbox/ingress custody. Queued, in-progress, uncertain unsettled, ancestry-linked
and recent historical provider work continues to protect its node. The
64-node retention target can therefore be exceeded while these protections
remain. Use authenticated inspection and explicit retirement when disposition
is required; do not delete registrations or nodes to force the count down.

For an already missing or inactive local target, distinguish an enabled
registration from a retained outbox target. Gone-target admission may disable
the registration and stop retrying a known-unaccepted job, but cannot discharge
prior uncertain acceptance. Explicit retirement remains the recovery action for
unsettled work, including a target whose node is already absent. Re-enabling
requires a provisioned active owned node and an active device; it does not
replay historical payloads. See the [aggregate evidence and attribution limits](
evidence/legacy-push-targets-2016.md).

Explicit XEP-0357 disable can also revoke a legacy registration whose local
provider node is missing or belongs to another owner. It still removes only
the requesting owner's exact service/node scope, and does not mutate another
owner's provider node, devices or work. Provider locks apply only to existing
owned nodes; an absent node must not prevent the user from disabling that
registration.

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
| `xmpp.push.registration.missing_node` | Enabled registrations at the configured local service without a provider node owned by the registered owner. |
| `xmpp.push.registration.inactive_node` | Enabled registrations at the configured local service whose owned provider node is disabled. |
| `xmpp.push.registration.snapshot_at` | Unix seconds of the last successful configured-local registration inventory. |

Count gauges use annotation units `{job}` and `{node}` so their Prometheus
names remain unsuffixed, such as `xmpp_push_queue_parked`. The snapshot gauge
uses `s` and translates to `xmpp_push_queue_snapshot_at_seconds`. The Rust test
checks exported units against the shared fixture. Verifying their translated
names against the actual recording rules requires the manual real OTLP
ingestion test below; a dimensionless gauge unit `1` would add `_ratio`.

Registration gauges carry no labels or identities. They publish complete
successful snapshots, including zeros, and freshness is recorded last. A
standalone store or absent host-registration schema cannot assert local-service
health and emits no registration snapshot. A failed query preserves prior
values and freshness. Interpret missing or stale registration snapshots as
unknown; compare replica values using the same fresh-per-replica `max` rule as
queue inventories. These counts distinguish enabled registrations from retained
outbox targets and do not grant automatic retirement authority.

Infrastructure PR CI currently lints rule syntax; it does not run
`rules/tests/run.sh`. Run the behavioral rule tests and OTLP translation check
manually when changing these metrics, the shared fixture or recording rules.
From the repository root, with an existing Prometheus 3.x distribution containing
both `promtool` and `prometheus`, run:

```sh
PROMETHEUS_DIR=/path/to/prometheus-distribution
PATH="$PROMETHEUS_DIR:$PATH" PROMETHEUS_BIN="$PROMETHEUS_DIR/prometheus" \
  sh infrastructure/waddle.cloud/rules/tests/run.sh
```

The runner always executes the `promtool` rule tests. It runs
`push_otlp_contract.py` only when `PROMETHEUS_BIN` is set; omitting that variable
does not verify OTLP translation. The OTLP check also requires Python 3 and uses
only local temporary files and a loopback listener.

Replicas sharing the database report the same inventory. First exclude each
replica's values when its own queue snapshot is older than five minutes, then
aggregate fresh values with `max`, never `sum`. This prevents an old high value
from overriding another replica's fresh zero. Missing or stale snapshots and
failed inventory reads mean current capacity is unknown; old or absent samples
must not be read as zero. The snapshot freshness alert waits five minutes after
the five-minute threshold. Exact identities remain in the restricted read-only
inventory and structured logs, outside metric attributes.
