# P2.1 pinned epoch and manifest catalog proof

Issue: #2046, #2116. Follow-up to the fixed dark read-only transaction entry in #2241. This work does not select a serving path.

## Gate and plan

1. Audit the physical checkout hook's pre-attestor dependency graph and bounded catalog materialization. Resolve any blocking defect before extending the transaction proof. Preserve independent operator pins and the required privileged mutation boundary.
2. On the existing exact pinned pool and bounded READ COMMITTED READ ONLY transaction, retain the physical lineage and complete V1030 ledger proofs. Before any epoch or manifest data read, prove the independently pinned epoch and manifest heaps through bounded catalog-only checks: names, OIDs, owners, ordinary permanent heap shape, built-in columns, constraints, safe expression dependencies, rules, RLS, inheritance, runtime/PUBLIC ACL, and exact pinned epoch primary-index structure and operator family. A manifest index without an independent pin may be checked structurally but is not an identity proof.
3. For each name-based `LOCK TABLE ONLY ... ACCESS SHARE`, perform catalog-only checks before the lock and recheck pinned identities and relevant shape after any wait with a new READ COMMITTED statement. Keep lineage → ledger → epoch → manifest lock order and acknowledge rollback before returning. Expose no raw transaction, data row, or serving capability.
4. Add PG17 positive and hostile tests for wrong pins, catalog/ACL/expression/RLS/index drift, name replacement during a lock wait, lock order, timeout and cancellation cleanup. Run focused regressions, formatting, scoped strict Clippy, and independent integrated review before merge.

## Explicit limits

The new epoch and manifest checks will not read their rows, validate manifest contents or the whole 13-table guard profile, execute a new definer, fence ordinary epoch updates, convert production callers, or enable version 2 startup. The inherited physical proof executes its attestor definer. A future READ WRITE entry must resolve the R SELECT-only versus V1009 `FOR SHARE` contract with a pinned A-owned prelock or an enforced AX-first transition protocol. Privileged catalog, role, ACL and non-cooperating writes must remain independently quiescent throughout this dark observation.
