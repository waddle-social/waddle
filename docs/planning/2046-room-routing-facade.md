# #2046 B2a: typed room routing boundary

This slice puts the existing room route choice behind a typed service while preserving today's planning and immediate execution. It starts from current `main` and does not activate new custody, change the ordered relay wire, or move mutable room or connection state.

## Contract

- Planning resolves the current room claim without sending to a remote owner. A missing claim and a local owner continue through the local room path. Stale or failed claim lookup has an explicit unavailable result and records the existing planning failure. A remote claim freezes the existing relay target in the plan and effect intent.
- Immediate execution uses the existing MUC bridge and retains its bounded owner refresh, claim fence, attempt outcome, captured route, sender replies, and pending-frame behavior. A possibly committed attempt never falls back to local execution.
- The two entry points share a domain-facing room operation, but retain separate decisions because planning and execution have different side effects and completion semantics. No generic room/account/connection resolver is introduced here.

## Verification

Extend native room planning and dispatch tests for local, remote, missing, stale, and failed claims; remote planning without a bridge; attempted, unavailable and uncertain immediate outcomes; preserved identity and replies; and non-cluster behavior. Run formatting and strict Clippy, the focused suites, then the applicable workspace CI checks. Keep PostgreSQL-dependent tests in the required inventory where relevant.

The current immediate effect adapter still collapses some uncertain outcomes to unavailable. This slice preserves that contract and does not claim end-to-end uncertainty semantics.
