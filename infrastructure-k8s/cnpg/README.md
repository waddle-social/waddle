# CloudNativePG for Talos Kubernetes

This directory contains configuration for deploying the CloudNativePG operator to provide automated PostgreSQL database management on Kubernetes.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [Manual Installation (Phase 11)](#manual-installation-phase-11)
- [Creating PostgreSQL Clusters](#creating-postgresql-clusters)
- [Storage Configuration](#storage-configuration)
- [Backup and Recovery](#backup-and-recovery)
- [Monitoring](#monitoring)
- [Verification](#verification)
- [Troubleshooting](#troubleshooting)
- [Security Best Practices](#security-best-practices)
- [Limitations](#limitations)
- [Files in This Directory](#files-in-this-directory)
- [References](#references)

## Architecture Overview

**CloudNativePG Components:**
- **Operator:** Manages PostgreSQL cluster lifecycle (provisioning, scaling, failover)
- **Cluster CRD:** Declarative PostgreSQL cluster definition
- **Instances:** PostgreSQL pods (primary + replicas) managed by the operator

**PostgreSQL Cluster Topology:**
```
┌─────────────────────────────────────────────────────────────────┐
│                     PostgreSQL Cluster Flow                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. User creates Cluster resource                                │
│       ↓                                                          │
│  2. CloudNativePG operator receives Cluster spec                 │
│       ↓                                                          │
│  3. Operator creates StatefulSet with PVCs                       │
│       ↓                                                          │
│  4. Proxmox CSI provisions volumes (proxmox-csi StorageClass)   │
│       ↓                                                          │
│  5. PostgreSQL pods start (1 primary + N replicas)               │
│       ↓                                                          │
│  6. Operator creates Services (rw, ro, r) and Secrets            │
│       ↓                                                          │
│  7. Streaming replication established between primary/replicas   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Integration with Cluster Infrastructure:**
```
┌─────────────────────────────────────────────────────────────────┐
│                     Talos Kubernetes Cluster                     │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    cnpg-system namespace                  │   │
│  │  ┌─────────────────────────────────────────────────────┐ │   │
│  │  │              CloudNativePG Operator                 │ │   │
│  │  │        (watches Cluster resources in all NS)        │ │   │
│  │  └──────────────────────┬──────────────────────────────┘ │   │
│  └─────────────────────────┼────────────────────────────────┘   │
│                            │                                     │
│  ┌─────────────────────────┼────────────────────────────────┐   │
│  │              Application Namespace (e.g., app-db)         │   │
│  │  ┌──────────────────────▼──────────────────────────────┐ │   │
│  │  │                   Cluster Resource                   │ │   │
│  │  └──────────────────────┬──────────────────────────────┘ │   │
│  │                         │                                 │   │
│  │  ┌──────────────────────▼──────────────────────────────┐ │   │
│  │  │  ┌─────────┐  ┌─────────┐  ┌─────────┐             │ │   │
│  │  │  │Primary  │  │Replica  │  │Replica  │ StatefulSet │ │   │
│  │  │  │ (rw)    │──│ (ro)    │──│ (ro)    │             │ │   │
│  │  │  └────┬────┘  └────┬────┘  └────┬────┘             │ │   │
│  │  └───────┼────────────┼───────────┼───────────────────┘ │   │
│  │          │            │           │                      │   │
│  │  ┌───────▼────────────▼───────────▼───────────────────┐ │   │
│  │  │           PersistentVolumeClaims (PVCs)            │ │   │
│  │  │         storageClass: proxmox-csi                  │ │   │
│  │  └───────────────────────┬────────────────────────────┘ │   │
│  └──────────────────────────┼──────────────────────────────┘   │
│                             │                                    │
│                             ↓                                    │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │              Proxmox CSI → Proxmox Storage               │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**Services Created:**
| Service | Purpose | Selector |
|---------|---------|----------|
| `<cluster>-rw` | Read-write (primary only) | `role: primary` |
| `<cluster>-ro` | Read-only (replicas only) | `role: replica` |
| `<cluster>-r` | Read (any instance) | All pods |

## Prerequisites

Before installing CloudNativePG, ensure:

1. **Talos cluster with Cilium CNI** (Phase 6 complete)
   - Nodes should be in `Ready` state
   - Verify: `kubectl get nodes`

2. **Proxmox CSI driver installed** (Phase 7 complete)
   - StorageClass `proxmox-csi` available
   - Verify: `kubectl get storageclass proxmox-csi`

3. **kubectl and Helm 3.x installed**
   ```bash
   kubectl version
   helm version
   ```

## Manual Installation (Phase 11)

### Step 1: Create Namespace

```bash
kubectl create namespace cnpg-system
```

### Step 2: Add Helm Repository

```bash
helm repo add cnpg https://cloudnative-pg.github.io/charts
helm repo update
```

### Step 3: Install CloudNativePG Operator

```bash
cd infrastructure-k8s/cnpg

helm install cloudnative-pg cnpg/cloudnative-pg \
  --version 0.22.1 \
  --namespace cnpg-system \
  --values helm-values.yaml
```

### Step 4: Verify Installation

```bash
# Check operator pod
kubectl get pods -n cnpg-system

# Expected output:
# NAME                              READY   STATUS    RESTARTS   AGE
# cloudnative-pg-xxxxxxxxx-xxxxx    1/1     Running   0          1m

# Check CRDs installed
kubectl get crds | grep cnpg

# Expected CRDs:
# backups.postgresql.cnpg.io
# clusters.postgresql.cnpg.io
# poolers.postgresql.cnpg.io
# scheduledbackups.postgresql.cnpg.io
# clusterimagecatalogs.postgresql.cnpg.io
# imagecatalogs.postgresql.cnpg.io

# Check operator logs
kubectl logs -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg
```

## Creating PostgreSQL Clusters

### Basic Cluster (3 Instances)

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: my-pg-cluster
  namespace: app-db
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 10Gi
  bootstrap:
    initdb:
      database: myapp
      owner: myuser
```

Apply and verify:
```bash
kubectl apply -f cluster.yaml

# Watch cluster status
kubectl get cluster -n app-db -w

# Check pods (1 primary + 2 replicas)
kubectl get pods -n app-db -l postgresql

# Check services
kubectl get svc -n app-db
```

### Cluster with Resource Limits

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: production-pg
  namespace: app-db
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 50Gi
  resources:
    requests:
      cpu: 500m
      memory: 1Gi
    limits:
      cpu: 2
      memory: 4Gi
  postgresql:
    parameters:
      max_connections: "200"
      shared_buffers: "512MB"
      effective_cache_size: "1536MB"
      log_min_duration_statement: "1000"
```

### Cluster with Monitoring

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: monitored-pg
  namespace: app-db
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 10Gi
  monitoring:
    enablePodMonitor: true
    # Custom metrics queries (optional)
    # customQueriesConfigMap:
    #   - name: custom-queries
    #     key: queries.yaml
```

### Connection Information

After cluster creation, credentials are stored in Secrets:

```bash
# Superuser credentials (postgres user)
kubectl get secret <cluster>-superuser -n <namespace> -o jsonpath='{.data.password}' | base64 -d

# Application user credentials
kubectl get secret <cluster>-app -n <namespace> -o jsonpath='{.data.password}' | base64 -d

# Connection string
kubectl get secret <cluster>-app -n <namespace> -o jsonpath='{.data.uri}' | base64 -d
```

## Storage Configuration

### StorageClass Selection

CloudNativePG uses the `proxmox-csi` StorageClass configured in Phase 7:

```yaml
spec:
  storage:
    storageClass: proxmox-csi
    size: 10Gi
```

### Volume Expansion

**Note:** Volume expansion support depends on Proxmox CSI driver version.

```bash
# Check if expansion is supported
kubectl get storageclass proxmox-csi -o jsonpath='{.allowVolumeExpansion}'
```

### PVC Naming Convention

CloudNativePG creates PVCs with predictable names:
- `<cluster-name>-1` (primary)
- `<cluster-name>-2` (replica)
- `<cluster-name>-3` (replica)

## Backup and Recovery

### Barman Cloud Architecture

CloudNativePG uses Barman Cloud for backup to S3-compatible object stores:

```
┌─────────────────────────────────────────────────────────────────┐
│                        Backup Architecture                       │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  PostgreSQL Primary                                              │
│       │                                                          │
│       ├── WAL Archiving (continuous) ──► S3 Bucket (WAL files)  │
│       │                                                          │
│       └── Base Backup (scheduled) ───► S3 Bucket (data files)   │
│                                                                  │
│  Recovery Options:                                               │
│       ├── Point-in-Time Recovery (PITR)                         │
│       ├── Latest backup restore                                  │
│       └── Clone from existing cluster                            │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### S3 Credentials Setup

```bash
# Create Secret with S3 credentials
kubectl create secret generic s3-creds \
  --from-literal=ACCESS_KEY_ID=<your-access-key> \
  --from-literal=ACCESS_SECRET_KEY=<your-secret-key> \
  -n <namespace>
```

### Cluster with Backup Configuration

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: backed-up-pg
  namespace: app-db
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 10Gi
  backup:
    barmanObjectStore:
      destinationPath: s3://my-bucket/postgres-backups
      endpointURL: https://s3.waddle.social  # For MinIO or custom S3
      s3Credentials:
        accessKeyId:
          name: s3-creds
          key: ACCESS_KEY_ID
        secretAccessKey:
          name: s3-creds
          key: ACCESS_SECRET_KEY
      wal:
        compression: gzip
      data:
        compression: gzip
    retentionPolicy: "30d"
```

### Scheduled Backup

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: ScheduledBackup
metadata:
  name: daily-backup
  namespace: app-db
spec:
  schedule: "0 0 * * *"  # Daily at midnight
  backupOwnerReference: self
  cluster:
    name: backed-up-pg
  immediate: true
```

### On-Demand Backup

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Backup
metadata:
  name: manual-backup
  namespace: app-db
spec:
  cluster:
    name: backed-up-pg
```

### Point-in-Time Recovery

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: restored-pg
  namespace: app-db
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 10Gi
  bootstrap:
    recovery:
      source: backed-up-pg
      recoveryTarget:
        targetTime: "2024-01-15 10:30:00"
  externalClusters:
    - name: backed-up-pg
      barmanObjectStore:
        destinationPath: s3://my-bucket/postgres-backups
        s3Credentials:
          accessKeyId:
            name: s3-creds
            key: ACCESS_KEY_ID
          secretAccessKey:
            name: s3-creds
            key: ACCESS_SECRET_KEY
```

## Monitoring

### Prometheus Metrics

PostgreSQL metrics are exposed on port 9187. Enable PodMonitor for Prometheus Operator:

```yaml
spec:
  monitoring:
    enablePodMonitor: true
```

### Key Metrics

| Metric | Description |
|--------|-------------|
| `cnpg_pg_database_size_bytes` | Database size |
| `cnpg_pg_replication_lag` | Replication lag in bytes |
| `cnpg_pg_stat_activity_count` | Active connections |
| `cnpg_pg_stat_bgwriter_*` | Background writer stats |
| `cnpg_pg_locks_count` | Lock counts by type |

### Grafana Dashboard

CloudNativePG provides a Grafana dashboard. Import dashboard ID: `20417`

### Manual Metrics Check

```bash
# Port-forward to PostgreSQL metrics endpoint
kubectl port-forward -n <namespace> <pod-name> 9187:9187

# Query metrics
curl http://localhost:9187/metrics
```

## Verification

### Quick Test

```bash
# Apply namespace first
kubectl apply -f verification/namespace.yaml

# Deploy sample cluster
kubectl apply -f verification/sample-cluster.yaml

# Watch cluster creation
kubectl get cluster -n cnpg-test -w

# Check pods (wait for all to be Running)
kubectl get pods -n cnpg-test

# Check cluster status
kubectl describe cluster sample-pg-cluster -n cnpg-test

# Connect to PostgreSQL
kubectl exec -it sample-pg-cluster-1 -n cnpg-test -- psql -U postgres -d testdb

# Verify replication
kubectl exec -it sample-pg-cluster-1 -n cnpg-test -- psql -U postgres -c "SELECT * FROM pg_stat_replication;"

# Cleanup
kubectl delete -f verification/sample-cluster.yaml
kubectl delete -f verification/namespace.yaml
```

### Test Failover

```bash
# Identify primary
kubectl get pods -n cnpg-test -l role=primary

# Delete primary pod (operator will promote replica)
kubectl delete pod sample-pg-cluster-1 -n cnpg-test

# Watch new primary election
kubectl get pods -n cnpg-test -w

# Verify new primary
kubectl get pods -n cnpg-test -l role=primary
```

## Troubleshooting

### Operator Not Starting

```bash
# Check operator pod status
kubectl describe pod -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg

# Check operator logs
kubectl logs -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg

# Common causes:
# - RBAC issues (check ClusterRole/ClusterRoleBinding)
# - Resource limits too low
# - Control plane tolerations missing
```

### Cluster Not Ready

```bash
# Check cluster status
kubectl describe cluster <name> -n <namespace>

# Check pod events
kubectl describe pod <cluster>-1 -n <namespace>

# Check PVC status
kubectl get pvc -n <namespace>

# Common causes:
# - PVC provisioning failed (check Proxmox CSI logs)
# - Storage quota exceeded
# - Invalid Cluster spec
```

### Connection Failures

```bash
# Verify services exist
kubectl get svc -n <namespace>

# Check service endpoints
kubectl get endpoints <cluster>-rw -n <namespace>

# Test connection from within cluster
kubectl run -it --rm psql --image=postgres:16 --restart=Never -- \
  psql -h <cluster>-rw -U postgres -d <database>

# Check secrets
kubectl get secret <cluster>-superuser -n <namespace>
```

### Replication Issues

```bash
# Check replication status
kubectl exec -it <cluster>-1 -n <namespace> -- \
  psql -U postgres -c "SELECT * FROM pg_stat_replication;"

# Check replication lag
kubectl exec -it <cluster>-1 -n <namespace> -- \
  psql -U postgres -c "SELECT client_addr, state, sent_lsn, replay_lsn FROM pg_stat_replication;"

# Check PostgreSQL logs
kubectl logs <pod> -n <namespace> -c postgres
```

### Backup Failures

```bash
# Check backup status
kubectl describe backup <backup-name> -n <namespace>

# Check scheduled backup status
kubectl describe scheduledbackup <name> -n <namespace>

# Common causes:
# - Invalid S3 credentials
# - Network connectivity to S3
# - Insufficient permissions on S3 bucket
# - Full storage bucket
```

## Security Best Practices

### Credentials Management

1. **Never commit PostgreSQL credentials to Git**
   - Credentials are auto-generated in Kubernetes Secrets
   - Use sealed-secrets or external-secrets for GitOps

2. **Rotate credentials regularly**
   ```bash
   # Update password in Secret
   kubectl patch secret <cluster>-superuser -n <namespace> \
     -p '{"data":{"password":"'$(echo -n "new-password" | base64)'"}}'
   ```

### TLS Configuration

Enable TLS for PostgreSQL connections:

```yaml
spec:
  certificates:
    serverTLSSecret: pg-server-tls
    serverCASecret: pg-ca-tls
    clientCASecret: pg-client-ca
```

### Network Policies

Restrict PostgreSQL access:

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: pg-access
  namespace: app-db
spec:
  podSelector:
    matchLabels:
      postgresql: sample-pg-cluster
  policyTypes:
    - Ingress
  ingress:
    - from:
        - namespaceSelector:
            matchLabels:
              name: app
        - podSelector:
            matchLabels:
              app: myapp
      ports:
        - port: 5432
```

### RBAC

Limit who can manage PostgreSQL clusters:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: pg-admin
  namespace: app-db
rules:
  - apiGroups: ["postgresql.cnpg.io"]
    resources: ["clusters"]
    verbs: ["get", "list", "watch", "create", "update", "delete"]
```

## Limitations

**CloudNativePG Operator Limitations:**

| Feature | Support |
|---------|---------|
| PostgreSQL 12-17 | ✅ Yes |
| High Availability | ✅ Yes (primary + replicas) |
| Automatic Failover | ✅ Yes |
| Continuous Backup | ✅ Yes (Barman Cloud) |
| Point-in-Time Recovery | ✅ Yes |
| Connection Pooling | ✅ Yes (PgBouncer via Pooler) |
| Volume Expansion | ⚠️ Depends on CSI driver |
| Multi-Region Clusters | ❌ No (single cluster) |
| Cross-Cluster Replication | ⚠️ External clusters only |

**Performance Considerations:**
- Single operator replica (not HA for operator itself)
- Network latency affects replication
- Storage performance impacts IOPS

## Files in This Directory

| File | Description |
|------|-------------|
| `README.md` | This documentation file |
| `helm-values.yaml` | Helm chart values for CloudNativePG operator |
| `kustomization.yaml` | Kustomization for Flux GitOps (Phase 11) |
| `verification/namespace.yaml` | Test namespace for verification |
| `verification/sample-cluster.yaml` | Sample PostgreSQL cluster for testing |

## References

- [CloudNativePG Documentation](https://cloudnative-pg.io/documentation/)
- [CloudNativePG GitHub](https://github.com/cloudnative-pg/cloudnative-pg)
- [Helm Chart Repository](https://github.com/cloudnative-pg/charts)
- [Cluster API Reference](https://cloudnative-pg.io/documentation/current/cloudnative-pg.v1/)
- [Backup and Recovery](https://cloudnative-pg.io/documentation/current/backup/)
- [Monitoring](https://cloudnative-pg.io/documentation/current/monitoring/)
- [PostgreSQL Documentation](https://www.postgresql.org/docs/)
