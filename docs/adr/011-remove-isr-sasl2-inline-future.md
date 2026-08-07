# ADR-011: Remove XEP-0397 ISR; Fast Reconnect via Future Inline SASL2 Resume

## Status

Accepted

## Date

2026-08-07

## Context

Issue #1631 identified a draft security advisory, GHSA-5687-26jr-g8vv,
severity Medium, CWE-312 (*Cleartext Storage of Sensitive Information*).
XEP-0397 Instant Stream Resumption (ISR) bearer tokens were stored in
cleartext at rest in Postgres: `clustering_isr_tokens.token TEXT NOT NULL`.
The feature was advertised live in the clustered release. The exposure is
gated by Postgres read access; it is not remotely exploitable by an
unauthenticated network attacker.

ISR has also been unstable in project history. It was fully removed in commit
`0988672e`, then reintroduced roughly 3.5 hours later in commit `a860c227`.
That churn makes an explicit, durable decision record necessary so ISR is not
silently reintroduced again.

ISR is not the capability that allows a dropped session to resume on an
arbitrary cluster node. That capability is provided by the Postgres-backed
clustering control plane through claims and ownership. ISR was only a
fast-reconnect optimization over that control plane. Without it, a cross-node
resume takes roughly 3–4 reconnect round trips instead of roughly 1.

## Decision

Remove XEP-0397 ISR in full from the codebase. Destroy the token store at
rest with global migration v10, which drops all three ISR-exclusive tables —
`clustering_isr_tokens`, `clustering_isr_revocation_fences`, and
`clustering_isr_sweep_state` — via idempotent `DROP TABLE IF EXISTS`
statements on both the SQLite and Postgres ledgers (the tables were created
outside the migration runner by the clustering store, so fresh and repaired
databases must tolerate their absence).

This resolves issue #1631 and is remediated by #1642 / PR #1665. The
additional reconnect round trips for cross-node resume are knowingly accepted;
no interim mitigation is introduced.

The future fast-reconnect path is a deferred roadmap item: XEP-0198 (Stream
Management) v1.6.3 §11's inline `<resume/>` inside a XEP-0388 (Extensible
SASL Profile / SASL2) `<authenticate/>` element. It is tracked as P3.4 under
umbrella issue #1662. This ADR does not implement that path. The codebase does
not currently implement XEP-0388 at all; advertising and implementing SASL2
is a separate future activation issue.

## Consequences

- ISR bearer tokens are no longer retained in cleartext in Postgres.
- Cluster-node-independent session resumption remains available through the
  Postgres-backed claims and ownership control plane.
- Cross-node resume loses ISR's roughly one-round-trip optimization and uses
  roughly 3–4 reconnect round trips until the deferred SASL2-inline-resume
  roadmap item is implemented.
- Future work must not reintroduce ISR. Any fast-reconnect work belongs to the
  separate XEP-0388/SASL2 activation boundary and the XEP-0198 inline-resume
  roadmap item.

## Alternatives Considered

- **Keep ISR with the existing token store.** Rejected: bearer tokens remain
  in cleartext at rest, and the feature's removal/reintroduction history shows
  that retaining it without this recorded decision would preserve instability.
- **Replace ISR immediately with SASL2 inline resume.** Rejected for this
  decision: XEP-0388 is not implemented, and its advertisement and activation
  are separately tracked future work under P3.4 / #1662.
