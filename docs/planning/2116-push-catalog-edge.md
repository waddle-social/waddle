# P2.1 Push registration catalog edge and joint-custody blockers

Status: grouped draft for #2116 under #2046. Restricted startup remains closed.

## Plan

1. Make the PostgreSQL Push registration owner constructor hold the catalog
   mutation fence from before its first catalog decision through table and
   index DDL on one bounded backend and transaction. Preserve SQLite behavior,
   owner-only repair semantics and attested runtime refusal.
2. Exercise the real public owner entry under concurrent catalog readers and
   owner attempts on PostgreSQL 17. Cover both winner orders, cancellation,
   partial owner state and bounded failure. A lost COMMIT must return no store
   and require separate reconciliation; this PR does not claim positive
   unknown-COMMIT recovery. Pin one persistent active schema and reject TEMP
   shadows before qualified DDL. The strict owner and ACL profile remains an
   independent runtime-attachment check.
3. Add combined-fixture witnesses for the two known joint Push custody
   incompatibilities: the legacy PubSub helper owner check conflicts with the
   H-owned helper, and the joint reader's direct R access to guarded ingress
   descendants conflicts with the A-authorized, R-denied role matrix. Preserve the stricter
   role graph and record the required definer design rather than weakening it.
4. Run focused PostgreSQL 17 and SQLite checks, formatting and strict scoped
   Clippy. Obtain an independent exact-head review before marking the PR ready.
5. Repair stale #2046 server test fixtures exposed by the broader test-target
   compile after the frame-provenance type change. Preserve the existing
   protocol and relay wire behavior, then compile the affected test targets.

This branch does not return functional Push handles to restricted startup,
change the selected role profile, activate workers, or relax either startup
refusal. Full joint custody, old-writer cutoff and all-store serving proof
remain separate acceptance gates. GitHub Actions and workflow files stay
disabled until the full #2046 implementation is finished.

## Local verification

The focused PostgreSQL 17.11 and SQLite registration target passed 4/4,
including both reader/owner lock orders, canceled DDL, a withheld COMMIT reply,
and panic cleanup. The combined restricted-profile fixture passed 1/1 with
the exact A/H/R refusal witnesses. The ordered-relay target passed 27/27 after
updating its synthetic frames to typed `Unknown` provenance. The other stale
test fixtures now extract the typed frame's stanza or provide that same
provenance; no protocol or wire implementation changed.

The server test-target compile passed with `test-support,clustering` enabled.
Formatting, `git diff --check`, and strict Clippy on all eight affected server
test targets passed. Existing warnings remain in vendor dependencies and some
unrelated server test code. These checks do not qualify the selected
restricted startup path, complete joint Push custody, or satisfy #2046's
release matrix.
