# Endpoint, outbound and actor custody site review

The original shard reviewed `5763923e0cb15d3bbab9cf53800e5b1bdd41e183`. Its review rows were carried forward to [`4219bdd71`](capture-4219bdd/README.md), matching the refreshed compiler sites. It is one part of the combined inventory; it does not claim discovery of unmarked callers or indirect edges.

The shard contains **1,428 of 1,428 profile-specific rows**, with **0 omitted** and **0 extra**. These represent 745 distinct tag/path/line/column/target keys after removing the profile; default has 683 rows and clustering has 745. Each row retains the full profile and compiled-target key.

| Tag family | Default | Clustering | Total |
| --- | ---: | ---: | ---: |
| `ACTOROUT` | 130 | 130 | 260 |
| `CLAIM` | 23 | 45 | 68 |
| `EP` | 306 | 308 | 614 |
| `LIVEWS` | 24 | 24 | 48 |
| `OBACK` | 13 | 19 | 32 |
| `OBE` | 175 | 200 | 375 |
| `ROOM` | 3 | 10 | 13 |
| `ROOMEFFECT` | 3 | 3 | 6 |
| `WSOUT` | 2 | 2 | 4 |
| `WSREG` | 4 | 4 | 8 |

- The live WebSocket path uses a legacy 256-envelope item queue, while actor original-envelope custody and its byte budget are separate pre-cutover machinery. Actor-oriented labels describe that candidate, not activation on `/ws`.
- Admission and dequeue do not prove a socket write, durable settlement, or an end-to-end transfer of every envelope. A consumed permit may have an uncertain outcome. The acceptance token is signaled only on the explicitly reviewed writer/relay path.
- `current_claim` is an unlocked observation and does not by itself fence a write. The distinct post-pending-write operation carries a stronger ordering contract and delegates to store-specific implementations where required.
- The raw `source_class_hint` is not a definitive cfg classifier: for example `state.rs:1844` follows individual test-only fields but is itself an unconditional field. Generic actor peer interpretation is a candidate awaiting a bounded producer, not evidence that its lane is live.
- The nine profile-specific function import/re-export sites use `name-only` / `excluded-name-only`; they resolve names and do not call the functions. Type-only declarations and imports use `type-only` / `excluded-type-only`.

The shard was checked for exact raw-key coverage and duplicate keys, source text and column bounds, field order, citation locations, and row-level vocabulary. The later aggregate validator matched the complete manifest, both refreshed production captures, site shards, handlers and supplied indirect-edge reviews. Those checks cannot establish exhaustive discovery or runtime reachability.
