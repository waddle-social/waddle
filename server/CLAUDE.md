# Claude Code Guidelines

## Rust

- **Always run `cargo fmt` before every commit.** CI checks formatting and will reject unformatted code.
- Never allow unwrap
- Never add clippy allows in general, fix the code
- If something can be a trait, use a trait and plan for extension

## Clustering wire format

- **Bump the ordered-relay remote message id on every wire format
  change (#1597).** Any change to `RemoteStanzaEnvelope` **or to the
  reply types** (`OrderedRelayReply`, `OrderedRelayAck`,
  `OrderedRelayNack`, `OrderedRelayNackReason`, and anything they
  contain) — payload variants, recipient/lane shapes, claim fields,
  NACK reasons —
  MUST bump the version suffix in
  `#[kameo::remote_message("waddle.clustering.relay.deliver_ordered.vN")]`
  (`clustering/relay.rs`). An old peer then fails the ask with
  `UnknownMessage` (provably uncommitted → `UnsupportedEnvelope` NACK:
  sequence rolled back, channel kept, no sticky diversion). Reusing an
  old id makes the old peer fail mid-decode with `DeserializeMessage`,
  which is indistinguishable from a reply-decode failure after a
  committed effect and poisons the shared ordered channel for the whole
  mixed-version window. Reply types are covered for the same reason in
  the other direction: a reply-only format change lets a mixed-version
  receiver commit the delivery and then fail the sender's reply decode
  — the same maybe-committed hazard. The bump is what makes rolling
  deploys safe; a PR that changes any of these types without bumping
  the id must be rejected.

## Telemetry

- **New metrics go through the create-at-increment macros only**
  (`waddle_xmpp::counter_add!` / `waddle_xmpp::histogram_record!` in
  `crates/waddle-xmpp/src/telemetry/`). There is no standalone
  registration API and none may be added: instrument creation happens
  at the increment site, so a registered-but-never-wired metric cannot
  exist.
- **Alert-worthy counters are zero-registered at startup** (#1436), and
  that is not an exception to the rule above: the zero goes through the
  emitting helper's own `counter_add!` call site (a private
  `add(count: u64)` next to the helper), driven by
  `telemetry::reliability::register_reliability_counters()` from
  `waddle-server::telemetry::init`. A new counter whose alert reads
  `> N` on a healthy pod belongs in that registration path; a counter
  whose alert reads `== 0` ("did this ever tick") must stay
  unregistered. See the `telemetry` module docs.
- **The Prometheus text renderer is frozen.** Never add a new metric
  family to `crates/waddle-xmpp/src/prometheus.rs`; it is being retired
  family-by-family (#1330).
- Metric names are dot.case, the unit is UCUM on the instrument (never
  in the name), and attributes come exclusively from the enumerated
  allowlist in `telemetry/attributes.rs`. JIDs, room JIDs, stream ids,
  and message ids are never metric attributes — they belong on spans
  and logs. The cardinality budget is documented in the `telemetry`
  module docs.
- Metric tests assert exported samples through
  `telemetry::test_support` (the in-memory reader seam), never
  instrument internals.

## Runtime

This project uses **Bun** as its JavaScript/TypeScript runtime. Prefer Bun APIs over Node.js equivalents:

- Use `Bun.file()` and `Bun.write()` instead of `fs.readFileSync`/`fs.writeFileSync`
- Use `await Bun.file(path).exists()` instead of `fs.existsSync()`
- Use `bun test` for running tests
- Use `bun run` for scripts

## Code Organization

**Group by function, not by type.**

### Bad (grouped by type)
```
src/
├── controllers/
│   ├── user.ts
│   └── order.ts
├── services/
│   ├── user.ts
│   └── order.ts
├── types/
│   ├── user.ts
│   └── order.ts
└── validators/
    ├── user.ts
    └── order.ts
```

### Good (grouped by function)
```
src/
├── user/
│   └── index.ts      # controller, service, types, validators
├── order/
│   └── index.ts      # controller, service, types, validators
└── shared/
    └── index.ts      # truly shared utilities
```

When working on "user" functionality, everything is in one place. Related code changes together.

## Project Structure

- `crates/` - Rust crates for core functionality
- `docs/` - Project documentation
- `scripts/` - Development tooling
