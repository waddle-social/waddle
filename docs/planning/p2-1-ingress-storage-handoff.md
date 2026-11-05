# P2.1 dark ingress storage handoff

This #2046/#2116 slice joins the owner-prepared ingress storage family to the
same physically attested main pool without enabling restricted serving. The
startup `UnconvertedStores` refusal remains before listeners and constructors.

## Plan

1. Obtain an independent architecture gate for MAM selection, trusted owner
   role input, inherited-global URL provenance, claim fencing, lineage
   registration, and the `MamStorage` trait boundary. Preserve existing SQLite
   and legacy PostgreSQL behavior.
2. Add a dark attested MAM startup attachment using the existing
   `DatabaseMamStorage` and exact main-pool handle. Reject explicit or fallback
   MAM store locations before any raw probe or child pool. Keep clustered SM
   and all incomplete outbox/room authority paths rejected.
3. Exercise MAM alongside the existing inbox, pending-delivery, and portable
   SM attested attachments. Verify one-slot admission, physical identity,
   owner/schema/ACL and claim drift, no runtime DDL, cancellation, and
   SQLite/legacy behavior on disposable PostgreSQL 17 and SQLite.
4. Run formatting, scoped strict Clippy, and an independent adversarial review.
   Record the exact tests and any blocked production prerequisites in the PR.

No runtime role switch, endpoint binding, worker start, epoch transition,
primary fencing, or mailbox schema is in scope. The V1009 runtime-grant and
old-writer cutoff gates remain prerequisites for restricted serving.

## Architecture outcome

The independent gate cleared dark selector wiring only. MAM selection retains
URL provenance and requires an operator-supplied owner role and claim schema.
The attested variant shares the main pool and aliases MAM lineage to Global.
Cluster builds refuse this variant without live claim handles. Caller-owned
archive acceptance still uses its ingress transaction; the MAM trait adapter
commits each of its own operations separately. Restricted startup remains
closed until the remaining stores and rollout authority are proven.
