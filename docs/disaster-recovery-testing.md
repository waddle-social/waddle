# Disaster Recovery Testing

Regular testing of disaster recovery procedures to validate RTO/RPO targets and ensure recoverability.

## Purpose

Disaster recovery testing ensures:
- **Validation**: DR procedures work as documented
- **RTO/RPO Verification**: Recovery targets are achievable
- **Gap Identification**: Missing procedures or documentation
- **Team Readiness**: Staff familiarity with recovery procedures

## Recovery Objectives

| Component | RTO | RPO | Notes |
|-----------|-----|-----|-------|
| Kubernetes Cluster | 4 hours | 0 (stateless) | Full rebuild from CDKTF |
| Flux GitOps | 30 minutes | 0 (Git is source of truth) | Re-bootstrap from Git |
| PostgreSQL (CNPG) | 1 hour | 15 minutes | PITR from continuous backup |
| Certificates | 1 hour | N/A | Re-issue from Let's Encrypt |
| Secrets | 30 minutes | N/A | Manual recreation from secure storage |

## Test Scenarios

### 1. Flux Re-bootstrap Test (Monthly)

**Objective**: Verify GitOps can be restored from Git repository

**Frequency**: Monthly

**Procedure**:

```bash
# 1. Document current state
kubectl get kustomizations -A > /tmp/pre-test-kustomizations.txt
kubectl get helmreleases -A > /tmp/pre-test-helmreleases.txt

# 2. Uninstall Flux (TEST ONLY - use test cluster)
flux uninstall

# 3. Re-bootstrap Flux
flux bootstrap github \
  --owner=<org> \
  --repository=waddle-infra \
  --branch=main \
  --path=clusters/production \
  --personal

# 4. Wait for reconciliation
flux get kustomizations -A --watch

# 5. Verify all resources reconciled
flux get kustomizations -A
flux get helmreleases -A

# 6. Compare with pre-test state
diff /tmp/pre-test-kustomizations.txt <(kubectl get kustomizations -A)
```

**Success Criteria**:
- [ ] All Kustomizations reconcile within 30 minutes
- [ ] All HelmReleases deploy successfully
- [ ] No manual intervention required
- [ ] Application functionality verified

### 2. PostgreSQL Backup/Restore Test (Monthly)

**Objective**: Verify database can be restored from backup

**Frequency**: Monthly

**Procedure**:

```bash
# 1. Create test cluster with sample data
kubectl apply -f - <<EOF
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: dr-test-postgres
  namespace: dr-test
spec:
  instances: 1
  storage:
    size: 1Gi
    storageClass: proxmox-csi
EOF

# 2. Wait for cluster ready
kubectl wait --for=condition=Ready cluster/dr-test-postgres -n dr-test --timeout=300s

# 3. Insert test data
kubectl exec -it dr-test-postgres-1 -n dr-test -c postgres -- \
  psql -c "CREATE TABLE dr_test (id serial, data text, created_at timestamp default now());"
kubectl exec -it dr-test-postgres-1 -n dr-test -c postgres -- \
  psql -c "INSERT INTO dr_test (data) VALUES ('DR Test Data $(date)');"

# 4. Record test data
kubectl exec -it dr-test-postgres-1 -n dr-test -c postgres -- \
  psql -c "SELECT * FROM dr_test;" > /tmp/pre-backup-data.txt

# 5. Trigger backup
kubectl cnpg backup dr-test-postgres -n dr-test

# 6. Wait for backup completion
kubectl wait --for=condition=Complete backup/dr-test-postgres-<timestamp> -n dr-test --timeout=600s

# 7. Delete original cluster
kubectl delete cluster dr-test-postgres -n dr-test

# 8. Restore from backup
kubectl apply -f - <<EOF
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: dr-test-postgres-restored
  namespace: dr-test
spec:
  instances: 1
  storage:
    size: 1Gi
    storageClass: proxmox-csi
  bootstrap:
    recovery:
      source: dr-test-postgres
  externalClusters:
    - name: dr-test-postgres
      barmanObjectStore:
        # ... backup configuration
EOF

# 9. Verify data integrity
kubectl exec -it dr-test-postgres-restored-1 -n dr-test -c postgres -- \
  psql -c "SELECT * FROM dr_test;" > /tmp/post-restore-data.txt

diff /tmp/pre-backup-data.txt /tmp/post-restore-data.txt

# 10. Cleanup
kubectl delete namespace dr-test
```

**Success Criteria**:
- [ ] Backup completes within 15 minutes
- [ ] Restore completes within 1 hour
- [ ] All data intact after restore
- [ ] No data loss (RPO met)

### 3. Node Failure Test (Quarterly)

**Objective**: Verify cluster handles node failure gracefully

**Frequency**: Quarterly

**Procedure**:

```bash
# 1. Document current state
kubectl get nodes -o wide > /tmp/pre-test-nodes.txt
kubectl get pods -A -o wide > /tmp/pre-test-pods.txt

# 2. Select target node (worker, not control plane)
TARGET_NODE=$(kubectl get nodes -l '!node-role.kubernetes.io/control-plane' -o name | head -1)

# 3. Cordon node (prevent new scheduling)
kubectl cordon $TARGET_NODE

# 4. Drain node (evict pods)
kubectl drain $TARGET_NODE --ignore-daemonsets --delete-emptydir-data

# 5. Power off VM in Proxmox
# (Manual step - use Proxmox web UI)

# 6. Wait and observe pod rescheduling
kubectl get pods -A -o wide --watch

# 7. Verify cluster stability
kubectl get nodes
kubectl get pods -A | grep -v Running

# 8. Power on VM in Proxmox
# (Manual step)

# 9. Wait for node rejoin
kubectl get nodes --watch

# 10. Uncordon node
kubectl uncordon $TARGET_NODE

# 11. Verify workloads rebalance
kubectl get pods -A -o wide
```

**Success Criteria**:
- [ ] Pods rescheduled within 5 minutes
- [ ] No service disruption (multi-replica workloads)
- [ ] Node rejoins cluster after power-on
- [ ] Workloads remain healthy throughout

### 4. Cluster Rebuild Test (Annually)

**Objective**: Verify complete cluster can be rebuilt from scratch

**Frequency**: Annually (use dedicated test environment)

**Procedure**:

```bash
# 1. Document current state
kubectl get all -A > /tmp/cluster-state.txt
kubectl get secrets -A --show-labels > /tmp/secrets-list.txt

# 2. Backup critical secrets
# (Document manually created secrets that need recreation)

# 3. Destroy cluster VMs in Proxmox
# (Manual step - ONLY in test environment)

# 4. Rebuild infrastructure with CDKTF
cd infrastructure
npm run deploy

# 5. Bootstrap Talos
# (Follow Talos setup documentation)

# 6. Bootstrap Flux
flux bootstrap github \
  --owner=<org> \
  --repository=waddle-infra \
  --branch=main \
  --path=clusters/production \
  --personal

# 7. Recreate secrets
kubectl create secret generic <secret-name> ...

# 8. Wait for full reconciliation
flux get kustomizations -A --watch

# 9. Verify all applications functional
# (Application-specific verification)

# 10. Run end-to-end tests
# (Application test suite)
```

**Success Criteria**:
- [ ] Cluster rebuilt within 4 hours (RTO)
- [ ] All infrastructure components deployed
- [ ] Applications functional after rebuild
- [ ] Data restored from backups (databases)

## Test Checklist

Use this checklist for each DR test:

### Pre-Test
- [ ] Schedule test with team
- [ ] Notify stakeholders
- [ ] Document current state
- [ ] Verify backup availability
- [ ] Prepare rollback plan

### During Test
- [ ] Record start time
- [ ] Document each step performed
- [ ] Note any deviations from procedure
- [ ] Capture screenshots/logs

### Post-Test
- [ ] Record end time
- [ ] Calculate RTO achieved
- [ ] Verify data integrity (RPO)
- [ ] Document issues encountered
- [ ] Update runbooks based on findings
- [ ] Schedule next test

## Test Results Template

```markdown
## DR Test Report

**Test Type**: [Flux Re-bootstrap | PostgreSQL Backup/Restore | Node Failure | Cluster Rebuild]
**Date**: YYYY-MM-DD
**Duration**: X hours Y minutes
**Participants**: [names]

### Results

| Metric | Target | Achieved | Status |
|--------|--------|----------|--------|
| RTO | X hours | Y hours | Pass/Fail |
| RPO | X minutes | Y minutes | Pass/Fail |

### Issues Encountered

1. [Issue description]
   - **Impact**: [description]
   - **Resolution**: [description]
   - **Action Item**: [description]

### Improvements Identified

1. [Improvement]
2. [Improvement]

### Next Steps

- [ ] Update runbook [X]
- [ ] Schedule next test: YYYY-MM-DD
```

## References

- [Flux Workflow](flux-workflow.md) - Disaster Recovery section
- [CloudNativePG Setup](cloudnativepg-setup.md) - Backup and Recovery section
- [Runbooks](runbooks/README.md) - Operational procedures
