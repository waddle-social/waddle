# Kameo remote bounds

The umbrella requires capacity qualification to include "Kameo remote
command admission, retained requests and spawned tasks, not just
application mailboxes", and names an unbounded remote command channel and
spawned request tasks in `server/vendor/kameo/src/remote/`. This record
states what bounds remote actor messaging today, what does not, and the
change that would close each gap. It is from reading source at this
commit; nothing was measured and no code was changed.

`K/` is `server/vendor/kameo/src/remote/`, `S/` is
`server/crates/waddle-server/src/`.

## What is bounded

| Bound | Value | Where | Configured by |
|---|---|---|---|
| Sender-side request timeout | 30 s | `S/clustering/swarm.rs:271`, `K/messaging.rs` `Config` | `WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS` |
| Receiver-side reply timeout | 20 s | per ask, `RelayHandle::reply_timeout` | `WADDLE_CLUSTERING_REPLY_TIMEOUT_MS` |
| Receiver-side mailbox timeout | 5 s | per ask, `RelayHandle::mailbox_timeout` | `WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS` |
| Concurrent request-response streams | 256 | `S/clustering/swarm.rs:272` | `WADDLE_CLUSTERING_MAX_CONCURRENT_STREAMS` |
| Request and response size | set from configuration | `S/clustering/swarm.rs:273-274` | `WADDLE_CLUSTERING_MAX_REQUEST_BYTES`, `WADDLE_CLUSTERING_MAX_RESPONSE_BYTES` |
| Ordered relay channels tracked | 8192 | `S/clustering/ordered_relay.rs:21` | constant |

`S/config.rs` requires all three timeouts to be non-zero, the reply and
mailbox timeouts to fit under the request timeout, and the stream and size
limits to be non-zero.

The stream limit is libp2p request-response's limit on concurrent streams.
It bounds how many requests are on the wire at once. It does not bound how
many are waiting to be put there, nor how many handler tasks the receiver
runs once the requests have been read.

## What is not bounded

1. **The swarm command channel.** `K/behaviour.rs:51-52,74` creates
   `mpsc::unbounded_channel::<SwarmCommand>()`. Every local `ask`, `tell`,
   lookup and registration is a command; `SwarmSender::send`
   (`K/swarm.rs:378-386`) never waits and never refuses. `Behaviour::poll`
   (`K/behaviour.rs:410`) takes one command per poll. A producer that
   outruns the swarm task grows this queue without limit, each entry
   holding its serialized payload.
2. **Retained outbound requests.** `K/messaging.rs:422` keeps
   `requests: HashMap<RequestId, (PeerId, Option<oneshot::Sender<…>>)>`,
   one entry per request handed to libp2p until its response, failure or
   the 30 s timeout. Requests beyond the stream limit wait inside libp2p's
   handler. The retained set is bounded only by send rate × request
   timeout.
3. **Inbound handler tasks.** `K/messaging.rs:1008-1095`
   (`handle_incoming_request`) spawns one task into `join_set`
   (`K/messaging.rs:423`) per inbound `Ask`, `Tell`, `Link`, `Unlink` and
   `SignalLinkDied`, with no admission check. Each `Ask` task lives up to
   mailbox timeout + reply timeout. The count is bounded only indirectly,
   by the stream limit per connection times the number of connections.
4. **Local requests.** `request_with_reply` (`K/messaging.rs:897-921`)
   spawns a task for a request addressed to the local peer, with no limit.
5. **Link-death fan-out.** `K/behaviour.rs:438-460` spawns one task per
   closed connection that signals every linked actor through an unbounded
   `FuturesUnordered`.

First-party code adds no bound in front of these: `RelayHandle`
(`S/clustering/relay/implementation.rs:1609`) carries the two timeouts and
no in-flight limit, and its 31 `ask`/`tell` call sites go straight to the
remote actor reference.

## Why no bound was added here

Each bound introduces a new outcome at the sender: "refused before
anything was sent". The ordered relay distinguishes outcomes that prove no
effect (retry the same sequence, or fall back) from outcomes that do not
(`ConnectionClosed`, a timeout); `server/vendor/kameo/WADDLE-PATCH.md`
records why a synthetic `DialFailure` for a cancelled request would
wrongly authorize a no-effect fallback. A refusal at admission is a proven
no-effect, but it has to be carried as its own error and classified in
`S/clustering/relay/` and `S/clustering/ordered_relay*`, which are being
rewritten. A bound added without that classification either blocks
callers that hold locks, or surfaces as an existing error with the wrong
meaning. The umbrella's order is also to bound and measure first-party
concurrency before deciding on a library change.

## The change, in order

1. **Measure.** Record the three quantities before choosing limits:
   command-queue depth (sample `cmd_rx.len()` in `Behaviour::poll`),
   retained outbound requests (`requests.len()`), and inbound tasks
   (`join_set.len()`), each as a histogram through a small hook the vendor
   patch exposes and `S/clustering/swarm.rs` records with the telemetry
   macros. Run the two-replica workload of package A, including a peer
   that stops reading.
2. **Bound first-party concurrency.** Give `RelayHandle` a per-peer
   semaphore for in-flight asks, sized at or below
   `max_concurrent_streams`, acquired with the mailbox timeout. A timeout
   there returns a new `RelayAskError` variant that states nothing was
   sent. With every remote ask behind it, libp2p's waiting queue and
   `requests` are bounded by the permit count, and the command queue by
   permits plus registry traffic. Classify the new variant in the ordered
   relay sender as retry-same-sequence.
3. **Bound inbound tasks in the vendor patch.** In
   `handle_incoming_request`, when `join_set.len()` is at a configured
   limit, answer at once instead of spawning. The answer needs a
   `RemoteSendError` variant that means "not admitted, not executed";
   `MailboxFull` is the closest existing one and already means no effect
   for `tell`, but reusing it hides the cause. A new variant changes the
   wire encoding of `RemoteSendError`, so it ships with a coordinated
   cutover and, where a relay message's reply type embeds it, with the
   version bump `server/AGENTS.md` requires.
4. **Bound the command channel in the vendor patch** only if step 1 shows
   it growing with step 2 in place: replace the unbounded channel with
   `mpsc::channel(n)`, make `SwarmSender::send` a `try_send`, and resolve a
   refused command's reply channel with `RemoteSendError::SwarmNotBootstrapped`'s
   sibling for a full queue. Commands without a reply channel
   (registrations, link signals) must not be dropped: they keep an
   unbounded side channel or block their caller.
5. Record the measured limits in the capacity qualification, with the
   topology and CPU limits they were measured under.

Every vendor change is a local difference from kameo 0.20.0 and belongs in
`server/vendor/kameo/WADDLE-PATCH.md`.
