# Claude Code Guidelines

## Rust

- **Always run `cargo fmt` before every commit.** CI checks formatting and will reject unformatted code.
- Never allow unwrap
- Never add clippy allows in general, fix the code
- If something can be a trait, use a trait and plan for extension

## Telemetry

- **New metrics go through the create-at-increment macros only**
  (`waddle_xmpp::counter_add!` / `waddle_xmpp::histogram_record!` in
  `crates/waddle-xmpp/src/telemetry/`). There is no standalone
  registration API and none may be added: instrument creation happens
  at the increment site, so a registered-but-never-wired metric cannot
  exist.
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
