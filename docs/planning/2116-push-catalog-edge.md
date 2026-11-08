# P2.1 Push registration catalog edge and joint-custody blockers

Status: grouped draft for #2116 under #2046. Restricted startup remains closed.

## Plan

1. Make the PostgreSQL Push registration owner constructor hold the catalog
   mutation fence from before its first catalog decision through table and
   index DDL on one bounded backend and transaction. Preserve SQLite behavior,
   owner-only repair semantics and attested runtime refusal.
2. Exercise the real public owner entry under concurrent catalog readers and
   owner attempts on PostgreSQL 17. Cover both winner orders, cancellation,
   partial owner state, bounded failure and positive recovery of an uncertain
   commit. Check that schema qualification and role separation prevent TEMP or
   cross-schema shadowing.
3. Add combined-fixture witnesses for the two known joint Push custody
   incompatibilities: the legacy PubSub helper owner check conflicts with the
   H-owned helper, and the joint reader's direct R access to guarded ingress
   descendants conflicts with the A-owned role matrix. Preserve the stricter
   role graph and record the required definer design rather than weakening it.
4. Run focused PostgreSQL 17 and SQLite checks, formatting and strict scoped
   Clippy. Obtain an independent exact-head review before marking the PR ready.

This branch does not return functional Push handles to restricted startup,
change the selected role profile, activate workers, or relax either startup
refusal. Full joint custody, old-writer cutoff and all-store serving proof
remain separate acceptance gates. GitHub Actions and workflow files stay
disabled until the full #2046 implementation is finished.
