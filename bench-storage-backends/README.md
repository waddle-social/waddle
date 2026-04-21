# waddle-bench

Benchmark suite for XMPP stanza database backends. Simulates the Waddle MAM
(XEP-0313) workload against pluggable storage.

## Layout

```
crates/
  bench-core      # shared types, StanzaStore trait, workload generator, metrics
  bench-sqlite    # rusqlite WAL backend (writer thread + reader pool) -- DONE
  bench-postgres  # sqlx + postgres backend
  bench-clickhouse # clickhouse backend
  bench-duckdb    # TODO
  bench-runner    # CLI
```

`ArchivedMessage` and the `mam_messages` schema are byte-compatible with
`waddle/server/crates/waddle-xmpp/src/mam/storage.rs` so numbers produced by
this suite transfer directly to the Waddle server.

## Build

```
cargo build --release
cargo test
```

## Run

```
# SQLite, 10 concurrent users, 10s measured window
./target/release/bench-runner --backend sqlite --scale 10 --duration 10s

# Postgres
./target/release/bench-runner --backend postgres \
  --postgres-url postgres://postgres:postgres@127.0.0.1:5432/postgres \
  --scale 10k --duration 60s

# ClickHouse
./target/release/bench-runner --backend clickhouse \
  --clickhouse-url http://127.0.0.1:8123 \
  --scale 10k --duration 60s

# 10 000 users
./target/release/bench-runner --backend sqlite --scale 10k --duration 60s

# 1 000 000 users
./target/release/bench-runner --backend sqlite --scale 1m  --duration 300s
```

Useful flags:

| flag | default | notes |
|---|---|---|
| `--scale` | `10` | `10`, `10k`, `1m`, or an explicit integer |
| `--duration` | `10s` | measured window, humantime (e.g. `30s`, `5m`) |
| `--warmup` | `0s` | pre-seeds the archive (capped at 200k rows) |
| `--ops-per-user-per-min` | `1.0` | Poisson arrival rate per session |
| `--p-write` | `0.2` | write probability (0.2 = 80/20 read/write) |
| `--reader-pool` | `32` | SQLite reader pool size |
| `--postgres-url` | unset | required when `--backend postgres` |
| `--postgres-max-connections` | `64` | Postgres pool size |
| `--clickhouse-url` | `http://127.0.0.1:8123` | ClickHouse HTTP endpoint |
| `--clickhouse-database` | `default` | ClickHouse database |
| `--clickhouse-user` | `default` | ClickHouse user |
| `--clickhouse-password` | empty | ClickHouse password |
| `--out` | `results` | output directory for `*.json` + `*.db` |

Each run drops two files in `--out`: a `*.db` SQLite file and a
`*.json` report. The report contains per-operation HDR-histogram
percentiles, total counts, DB size, peak RSS, and a per-second
throughput sample series.

## Workload model

Each of `--scale` sessions is a lightweight virtual user — we do **not** spawn
a tokio task per user (at 1 M that's ~1 GiB in task overhead alone). Instead a
single driver task ticks every 10 ms and draws a Poisson count of ops for
that interval at aggregate rate `sessions * ops_per_user_per_sec`. Ops are
stamped `Write` with probability `p_write`, drained by a fixed pool of
`num_cpus * 4` worker tasks hitting the store.

Reads are a weighted mix of three canonical MAM shapes:
* 60% time-range scan (`room_jid` + `timestamp` range, limit 100)
* 25% pagination (`room_jid` + id cursor, limit 50)
* 15% sender-filtered range (adds `from_jid` filter)

Writes are `groupchat` messages with `thread_id`, `origin_id`, `stanza_id`
populated — realistic enough to exercise all three indexes on `mam_messages`.

## Known ceilings

SQLite serialises writes through a single writer. At 1 M users × 0.2 write
probability × 1 op/user/min ≈ 3.3 k writes/sec — well within SQLite's NVMe
range. Pushing `--ops-per-user-per-min` higher will eventually saturate the
writer queue; the runner reports `backpressure` count in the JSON report
rather than crashing. That saturation point is what the Postgres and ClickHouse
backends need to beat.
