# Remote owner mirror reconciliation

An owner-side mirror is routing state for a socket hosted on another cluster
node. Retire it only after an authoritative node lookup proves that the socket
incarnation is missing, committed expired, or replaced by a different epoch,
after complete cluster membership and every node's own socket/session probe
prove the exact resource absent, or after an exact unregister request
established an owed retirement. A healthy node lease alone does not prove that
its socket still exists. Failed lookups, uncertain membership, and transport
unreachability do not establish absence.

## Schedule and latency

The dedicated `remote_owner_mirror` janitor runs every second, independently of
the five-minute empty-user actor reaper. Each page attempts at most 64 mirrors,
with a 30-second page deadline and a five-second per-entry ceiling. Slow work
skips missed ticks instead of producing a burst of catch-up pages.

Finite current/next rounds guarantee that arrivals cannot starve retained
mirrors. Only attempted entries rotate. Cancellation, lock contention, or the
page deadline preserves the unattempted suffix for the next page.

The tested healthy-dependency target is one complete 1,024-mirror round within
30 seconds after committed expiry, using the production one-second ticker and
fast actor/database replies. Such a fixed inventory requires 16 pages. This is
not a latency guarantee during backend outages or actor stalls: the per-entry
and page bounds preserve forward progress and safety, while errors remain
visible. Larger populations require more pages; ongoing arrivals enter a later
finite round. Time before a node's expiry is committed is outside this target.

One socket-node lease result is shared by mirrors on the same page. The cache
is discarded after the page, so expiry and epoch changes are observed again on
later pages. It never becomes a process-wide liveness cache.

Healthy-node mirrors are checked against locally hosted connections and
resumable XEP-0198 sessions on every unexpired peer, using the same read-only
presence probes as recovery. Remote mirrors and UserActor resource lists are
excluded from that evidence. Cleanup rechecks the exact registration and socket
incarnation under the per-resource registration gate before recording owed work.
Actor and connection-registry removal compare the old connection owner; a
rebound socket or successor registration must survive an old retry.

Current account ownership and occupancy generations govern publication, not
removal of obsolete local routing copies. Ownership moving to another node, or
a fresh bind advancing the generation before its registration rolls back, must
not pin an old mirror. Cleanup never rewrites occupancy authority or detaches a
remote socket. Any user-claim release uses the local actor's captured owner and
epoch, preserving foreign and successor claims. The expiry, exact unregister or
complete-absence evidence above remains required; an unavailable peer or unknown
membership cannot establish new healthy-node cleanup authority.

Routing reconciliation removes the connection-registry mirror and UserActor
resource and retries interrupted removal without requiring a new stanza.
Recovery retirement instead settles a frozen delivery obligation after proving
that it can no longer reach its resource. Recovery can make progress despite a
stale routing mirror, and its success does not prove that either routing copy
has been removed. Neither path grants authority to retire durable remote MUC
membership; that separate lifecycle remains tracked in #1825.

## Outcomes

`waddle.janitor.sweeps` uses `janitor="remote_owner_mirror"` and an enumerated
`outcome`:

- `completed`: the selected page completed its checks and retirements.
- `deferred`: work remains because of normal contention, an explicitly busy
  actor, uncertain presence/membership, or a page budget. This is
  progress/backlog information, not proof of absence.
- `failed`: a lease lookup, actor operation, or its dependency deadline failed.
  A retained mirror remains available for retry; failure never licenses removal.

The user-actor reaper independently retries its bounded convergence inventory.
A successful retry response with remaining work is deferred; actual failed
retry operations remain failed. A mirror retirement confirms routing cleanup,
not that a durable UserActor claim release has completed. Exact claim-release
retry remains owned by the user registry and its reaper.

The checked-in `JanitorHeartbeatStale` rule sums across outcomes per instance.
All three outcomes are evidence that a loop ran. Do not restore the old
minimum-over-outcomes rule from #1711: an idle failure series is not a stale
heartbeat.

## Progress and backlog metrics

All metrics use the existing create-at-emission OTel macros. No JID, room,
registration ID, or socket-node ID is a metric label.

| Metric | Meaning |
| --- | --- |
| `xmpp.remote_owner_mirror.attempts` | Attempt count by `live`, `retired`, `superseded`, `deferred`, or `failed`. |
| `xmpp.remote_owner_mirror.inventory` | Histogram samples of all retained mirrors after a page. |
| `xmpp.remote_owner_mirror.pending` | Histogram samples of locally known owed retirements after a page. |
| `xmpp.remote_owner_mirror.oldest_pending_age` | Histogram samples, in seconds, of the oldest locally known owed retirement; zero when none remain. |

These are sampled distributions, not instantaneous gauges. Use windowed
histogram count/sum or bucket queries with the collector's translated names.
Backlog observations are emitted only when the inventory lock is available;
missing samples during contention must not be interpreted as zero backlog.

Pending age starts when this process learns that retirement is owed. It does
not include time before expiry or exact-resource absence is detected, and it is not durable across a
restart. Same-JID successor registrations do not inherit the predecessor's
pending age. Persistent remote membership recovery is tracked separately in
[#1825](https://github.com/waddle-social/waddle/issues/1825), and the remote
join acknowledgement gap in [#1826](https://github.com/waddle-social/waddle/issues/1826).

Investigate sustained failures through lease-store and actor spans. A growing
pending-age distribution with deferred outcomes identifies a retirement that
is not converging even though the loop remains alive. Compare retirement
progress and inventory samples before treating an increasing inventory as a
leak: healthy remote sockets also occupy mirror entries.
