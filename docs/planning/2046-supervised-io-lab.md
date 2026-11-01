# P3.1 supervised I/O laboratory

This is a dark, test-only characterization for #2046 / #2137, stacked on
#2150. It neither creates a connection actor nor wires a transport into the
server. It owns no `WsConnState`, authenticated session, endpoint, SM state,
or durable obligation. No actor extraction, endpoint DDL, lane cutover, or
activation is authorized here; DEC-3, P2, and the coupled P3 gates still apply.

## Existing boundary

The current connection loop splits the WebSocket in
[`connection.rs`](../../server/crates/waddle-server/src/server/routes/websocket/connection.rs).
[`batch_write.rs`](../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs)
borrows the sink, stream, and `WsConnState` together: it can drain and process
SM acknowledgements while writing a batch. Moving only one of those mutable
pieces to a new owner would change that coordination. A future design must
resolve the complete protocol-state handoff and ordering before any extraction.

`SocketLifecycle::Detached` is a **binding cleanup** observation. In
[`reset_resume_attempt`](../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs),
a failed finalization can mark the attempted lifecycle detached while the
same physical socket continues and can bind a fresh session. It is not a join,
a close handshake, or proof of physical termination. The XEP WebSocket test
here separately verifies a failed resume followed by fresh binding, SM enable,
and traffic on the same client socket; its item-not-found schedule does not
inject a failure after lifecycle publication and does not itself observe
`SocketLifecycle::Detached`.

## Lab contract and observations

The integration test in
[`supervised_io`](../../server/crates/waddle-ws-test-support/tests/supervised_io.rs)
uses real loopback WebSocket streams split into reader and writer halves. Each
half moves to one task; the lab supervisor retains both `JoinHandle`s. A
controlled wrapper stalls before underlying `poll_ready` or after `start_send`
at `poll_flush`. The peer can observe a WebSocket close or loss of the
underlying connection once both tasks relinquish their halves.

| Schedule | What the test observes |
| --- | --- |
| Force interruption with readiness or flush stalled | The reader admits an input while the writer is stalled. Both results are joined; the queued tail never starts; the in-flight frame is respectively not attempted or attempted/uncertain. The peer observes close/drop. |
| Count or byte limit in either direction | A non-blocking failed admission stops this lab transport, and both tasks are joined. The limit covers queued **and in-flight** items, with independent count and payload-byte permits. |
| Requester cancellation after admission | The requester is joined as cancelled, but an already attempted writer operation remains uncertain; stopping the transport and joining the pumps are separate actions. |
| Reader panic or writer abort | The first result is observed, the surviving pump is interrupted, and both task results are joined. An abort request alone is not a join. |
| Graceful shutdown | The unstalled writer emits a WebSocket close, the peer reads it, and both tasks are joined. A separately tested stalled shutdown uses a deadline, aborts remaining tasks, and joins their results; no close handshake is claimed in that case. |
| Kameo control experiment | An actual Kameo actor with a bounded 256-entry mailbox is held in one turn. Filling all 256 slots rejects a control tell. An independent cancellation signal reaches that turn only because the handler cooperatively polls it. |

A transport admission is not durable custody. The experiment labels a frame
`NOT_ATTEMPTED` until the real sink's `start_send` is invoked, then `ATTEMPTED`
until its flush completes. After an error, abort, or cancellation following
that call the outcome remains uncertain; even a successful flush is not a
client acknowledgement. These labels are test instrumentation, not a new
public delivery contract. The lab's overload policy is to terminate the test
transport, not to discard or settle any durable obligation in production.
Payload bytes are bounded in addition to count, not allocator overhead,
WebSocket framing, kernel buffers, actor state, or total process memory.

A future supervisor must exclusively own the real reader and writer halves,
retain their handles across every retirement path, and collect **both** task
results before claiming local pump termination. `abort()`, channel closure,
registry removal, binding detachment, and actor death each leave that proof
incomplete. A deadline may bound the graceful phase, but abort must be followed
by joins and unresolved cleanup must remain discoverable. Peer-visible close
or drop is a separate observation from local join completion. The controlled
sinks exercise selected deterministic schedules, not all kernel buffering,
partial writes, scheduler races, peer implementations, process crashes, or
cross-node generation fences. The Kameo probe does not prove mailbox priority,
preemption, fairness, a bounded continuation design, or SM-control correctness.

## XEP-0198 constraints

The checkout has no `xeps/xep-0198.xml`; the official
[XSF source](https://github.com/xsf/xeps/blob/master/xep-0198.xml)
(blob `fc957a129d5479714db7bbf5626f4ec57b4272a1`) was reviewed, especially
Acks, Resumption, Errors, and Stream Closure. SM control elements are not
countable stanzas; an acknowledgement reports handled stanzas, not writer
queue admission or socket flush. A future scheduler must maintain the proven
contiguous handled prefix and replay-before-live ordering, while preserving
principal checks, failure responses, and distinction between a cleanly closed
stream and an unfinished resumable session. The test does not change any XMPP
wire or advertised feature.

## Remaining evidence

This lab does not exercise the published-lifecycle rollback over an actual
socket, production `batch_write` under the new pump design, durable replay
custody, remote failover, or a real control scheduler with protocol state.
Those remain prerequisites for a later design and adversarial review. The
source inventory and execution evidence must be checked independently of this
experiment; neither an isolated test nor this document authorizes activation.
