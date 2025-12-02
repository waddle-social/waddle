# PostgreSQL Down Runbook

## Alert

**Alert**: PostgreSQLDown, PostgreSQLInstanceNotHealthy, PostgreSQLReplicationLag, PostgreSQLBackupFailed
**Severity**: Critical
**Impact**: Database unavailable, application outage, potential data loss

## Overview

A CloudNativePG PostgreSQL cluster is experiencing issues. This affects all applications depending on the database and may impact data integrity.

## Diagnosis

### 1. Check Cluster Status

```bash
# List all PostgreSQL clusters
kubectl get clusters -A

# Get cluster status
kubectl describe cluster <cluster-name> -n <namespace>

# Check cluster phase
kubectl get cluster <cluster-name> -n <namespace> -o jsonpath='{.status.phase}'
```

### 2. Check Pod Status

```bash
# List cluster pods
kubectl get pods -n <namespace> -l cnpg.io/cluster=<cluster-name>

# Check pod details
kubectl describe pod <pod-name> -n <namespace>

# Check pod resource usage
kubectl top pods -n <namespace> -l cnpg.io/cluster=<cluster-name>
```

### 3. Check PostgreSQL Logs

```bash
# PostgreSQL container logs
kubectl logs <pod-name> -n <namespace> -c postgres

# Instance manager logs
kubectl logs <pod-name> -n <namespace> -c instance-manager

# Follow logs
kubectl logs <pod-name> -n <namespace> -c postgres -f
```

### 4. Check Operator Status

```bash
# Operator pods
kubectl get pods -n cnpg-system

# Operator logs
kubectl logs -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg

# Filter for specific cluster
kubectl logs -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg | grep <cluster-name>
```

### 5. Check Storage

```bash
# List PVCs
kubectl get pvc -n <namespace> -l cnpg.io/cluster=<cluster-name>

# Check PVC status
kubectl describe pvc <pvc-name> -n <namespace>

# Check storage class
kubectl get storageclass proxmox-csi
```

### 6. Check Replication Status

```bash
# Connect to primary and check replication
kubectl exec -it <primary-pod> -n <namespace> -c postgres -- \
  psql -c "SELECT client_addr, state, sent_lsn, write_lsn, flush_lsn, replay_lsn FROM pg_stat_replication;"

# Check replica lag
kubectl exec -it <replica-pod> -n <namespace> -c postgres -- \
  psql -c "SELECT pg_last_wal_receive_lsn(), pg_last_wal_replay_lsn(), pg_last_xact_replay_timestamp();"
```

## Common Causes

1. **Storage Failure**: PVC pending, disk full, CSI driver issues
2. **Resource Exhaustion**: OOM, CPU throttling
3. **Network Issues**: Pod-to-pod communication failure
4. **Primary Failure**: Unplanned failover needed
5. **Replication Issues**: Lag, broken replication
6. **Backup Failure**: S3/storage backend issues

## Remediation

### Scenario 1: Pod Pending (Storage Issue)

```bash
# Check PVC status
kubectl get pvc -n <namespace>

# If PVC pending, check CSI driver
kubectl get pods -n csi-proxmox

# Check CSI driver logs
kubectl logs -n csi-proxmox -l app=csi-proxmox-controller

# Verify Proxmox storage availability
# (Check Proxmox web UI for storage status)
```

**Fix PVC binding:**
```bash
# If PV exists but not bound, delete PVC and let operator recreate
kubectl delete pvc <pvc-name> -n <namespace>
# Operator will recreate PVC automatically
```

### Scenario 2: Pod OOMKilled

```bash
# Check if OOMKilled
kubectl describe pod <pod-name> -n <namespace> | grep -A 5 "Last State"

# Increase resources in cluster spec
kubectl edit cluster <cluster-name> -n <namespace>
```

Update resources:
```yaml
spec:
  resources:
    requests:
      memory: "1Gi"
    limits:
      memory: "2Gi"  # Increase this
```

### Scenario 3: Primary Failure

CloudNativePG handles automatic failover. If manual intervention needed:

```bash
# Check current primary
kubectl get cluster <cluster-name> -n <namespace> -o jsonpath='{.status.currentPrimary}'

# If failover is stuck, promote replica manually
kubectl cnpg promote <cluster-name> <replica-pod> -n <namespace>

# Or using kubectl exec
kubectl exec -it <replica-pod> -n <namespace> -c postgres -- \
  pg_ctl promote -D /var/lib/postgresql/data
```

### Scenario 4: High Replication Lag

```bash
# Check network between pods
kubectl exec -it <primary-pod> -n <namespace> -- \
  ping <replica-pod-ip>

# Check replication settings
kubectl exec -it <primary-pod> -n <namespace> -c postgres -- \
  psql -c "SHOW wal_level; SHOW max_wal_senders; SHOW wal_keep_size;"

# Increase wal_keep_size if needed
kubectl edit cluster <cluster-name> -n <namespace>
```

### Scenario 5: Backup Failure

```bash
# Check backup status
kubectl get backups -n <namespace>

# Check scheduled backup status
kubectl describe scheduledbackup <backup-name> -n <namespace>

# Check backup credentials (if using S3)
kubectl get secret <backup-secret> -n <namespace>

# Trigger manual backup
kubectl cnpg backup <cluster-name> -n <namespace>
```

### Scenario 6: Complete Cluster Failure

If the entire cluster is unrecoverable:

```bash
# Check available backups
kubectl get backups -n <namespace>

# Restore from backup
kubectl apply -f - <<EOF
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: <cluster-name>-restored
  namespace: <namespace>
spec:
  instances: 3
  storage:
    size: 10Gi
    storageClass: proxmox-csi
  bootstrap:
    recovery:
      source: <cluster-name>
  externalClusters:
    - name: <cluster-name>
      barmanObjectStore:
        destinationPath: s3://<bucket>/<path>
        # ... backup credentials
EOF
```

### Emergency: Connect Directly

If you need to perform emergency database operations:

```bash
# Port-forward to PostgreSQL
kubectl port-forward <pod-name> -n <namespace> 5432:5432

# Connect with psql
PGPASSWORD=$(kubectl get secret <cluster-name>-superuser -n <namespace> \
  -o jsonpath='{.data.password}' | base64 -d) \
  psql -h localhost -U postgres
```

## Verification

After remediation:

```bash
# Verify cluster is healthy
kubectl get cluster <cluster-name> -n <namespace>

# Verify all instances running
kubectl get pods -n <namespace> -l cnpg.io/cluster=<cluster-name>

# Verify replication
kubectl exec -it <primary-pod> -n <namespace> -c postgres -- \
  psql -c "SELECT * FROM pg_stat_replication;"

# Test application connectivity
kubectl exec -it <app-pod> -n <namespace> -- \
  psql -h <cluster-name>-rw -U <user> -d <database> -c "SELECT 1;"
```

## Escalation

- **5 minutes**: Check automatic failover status
- **15 minutes**: If failover fails, manual intervention
- **30 minutes**: Consider restore from backup
- **1 hour**: Escalate to Infrastructure Lead

## Related Documentation

- [CloudNativePG Setup](../cloudnativepg-setup.md)
- [Disaster Recovery Testing](../disaster-recovery-testing.md)
- [CloudNativePG Documentation](https://cloudnative-pg.io/docs/)
