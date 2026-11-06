# Source-marked dark SM transfer prerequisite for #2046

## Delivered boundary

The unselected SM custody candidate can copy a source-marked slot into its
existing candidate transfer journal and atomically record its transfer terminal.
It refuses an unbound or unmarked admission before sealing the gate, preserving
the payload for inventory and recovery. A generic completed attempt is not a
source marker. A retry requires the original slot, marker, terminal and exact
payload, resource and receipt time. ACK and transfer serialize on the gate.

The candidate schema also checks that a transfer copy and transfer terminal
refer to the same operation's marker and slot. The source handoff repository
updates the exact state-1 attempt and writes its marker in the same transaction.
These checks are a dark repository boundary: the disposable runtime role still
has raw DML and can bypass repository policy. In particular, a no-op update of
an earlier completed attempt followed by a direct marker insert can forge the
candidate marker. A restricted writer or definer authority remains necessary;
the candidate's trigger checks are not proof against arbitrary SQL by that role.

This journal is not `pending_delivery`, and a transfer terminal does not prove
pending acceptance, an offline copy, socket delivery or client acknowledgement.
There is no serving selector or registered migration in this slice.

## Pending transfer remains blocked

A recipient-wide pending row must not be accepted while a sibling can still
replay or begin a send. `sm_persistence/atomic_store.rs` writes drained ingress
entries through `insert_or_withhold` without the canonical lock or a check of a
recipient-wide acceptance. A delayed drain can therefore insert a replay entry
after an apparently empty sibling scan and pending acceptance. Process-local
drained queues are not enumerable from that scan. A coherent old-writer fence
or conversion, covering this path and other source authorities, is required
before accepting a real shared pending row. A generic effect receipt is not
typed pending proof and cannot settle a sibling's still-live replay obligation.

The subsequent group must bind both stores to one physically attested database,
schema and transaction; use the existing quota and FIFO rules; retain a typed,
recipient-wide acceptance and exact per-resource mapping after pending row
deletion; preserve exact source time while normalizing pending milliseconds; and
reconcile unknown COMMIT outcomes only by positive exact readback under the
current claim and sealed gate. It must also verify sibling concurrency, stale
writers, source and canonical GC, quota, ACK and native-process COMMIT loss.

Authenticated tombstones, global GC, old writer drain, live/relay/batch/detached
writer conversion, and serving selection remain separate gates.
