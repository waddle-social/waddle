# Proposed connection identity and ownership contract

Status: **draft for #2046 package A**. This is a planning document, not an
accepted ADR or authorization to change runtime behavior. Replica count and
numerical budgets remain owner decisions informed by #2106's local qualification.

## Plan

- Reconcile the current connection, node, resource, occupancy and SM identities
  with #2046 and #1662, including non-clustering restarts and epoch rotation.
- Propose and independently review a narrow node-local identity boundary for
  W1.3, preserving existing remote representation until its own migration.
- Record inherited requirements, current evidence, proposals and unresolved
  decisions separately. Refresh affected callers against current main rather than
  treating historical inventory counts as current.
- Request architecture, protocol and operations review before advancing this
  proposal. Package A remains open; a draft does not waive its budget or approval
  prerequisites.
