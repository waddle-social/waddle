# CloudNativePG Setup Guide

This guide provides comprehensive instructions for deploying and configuring CloudNativePG for automated PostgreSQL cluster management on Kubernetes.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Creating PostgreSQL Clusters](#creating-postgresql-clusters)
- [Connecting to Databases](#connecting-to-databases)
- [Backup and Recovery](#backup-and-recovery)
- [Monitoring](#monitoring)
- [High Availability](#high-availability)
- [Security](#security)
- [Troubleshooting](#troubleshooting)
- [References](#references)

## Overview

### What is CloudNativePG?

CloudNativePG is a Kubernetes operator for managing PostgreSQL clusters with:

- **Declarative Management:** Define PostgreSQL clusters as Kubernetes resources
- **High Availability:** Automatic failover with primary and replica topology
- **Continuous Backup:** WAL archiving and base backups to S3-compatible storage
- **Point-in-Time Recovery:** Restore to any point within retention window
- **Monitoring:** Prometheus metrics and Grafana dashboards
- **Connection Pooling:** PgBouncer integration via Pooler resources

### Key Features

| Feature | Description |
|---------|-------------|
| PostgreSQL 12-17 | Support for multiple PostgreSQL versions |
| Automatic Failover | Promotes replica on primary failure |
| WAL Archiving | Continuous backup to object storage |
| PITR | Point-in-time recovery capability |
| TLS | Encrypted connections and replication |
| Monitoring | Prometheus metrics on port 9187 |
| PgBouncer | Connection pooling via Pooler CRD |

### Version Information

- **Operator Version:** 1.25.0
- **Helm Chart Version:** 0.22.1
- **PostgreSQL Versions:** 12, 13, 14, 15, 16, 17

## Architecture

### Component Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                     CloudNativePG Architecture                   │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                    cnpg-system namespace                    │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │              CloudNativePG Operator                   │  │ │
│  │  │  - Watches Cluster resources in all namespaces        │  │ │
│  │  │  - Manages PostgreSQL instance lifecycle              │  │ │
│  │  │  - Handles failover and recovery                      │  │ │
│  │  └──────────────────────────────────────────────────────┘  │ │
│  └────────────────────────────────────────────────────────────┘ │
│                              │                                   │
│                              │ Watches/Manages                   │
│                              ▼                                   │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                  Application Namespace                      │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │                 Cluster Resource                      │  │ │
│  │  │  - instances: 3 (1 primary + 2 replicas)             │  │ │
│  │  │  - storage: proxmox-csi, 10Gi                        │  │ │
│  │  │  - backup: barmanObjectStore (optional)              │  │ │
│  │  └──────────────────────────────────────────────────────┘  │ │
│  │                              │                              │ │
│  │                              │ Creates                      │ │
│  │                              ▼                              │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │ ┌─────────┐ ┌─────────┐ ┌─────────┐                  │  │ │
│  │  │ │Primary  │ │Replica  │ │Replica  │  StatefulSet     │  │ │
│  │  │ │ (rw)    │ │ (ro)    │ │ (ro)    │  Pods            │  │ │
│  │  │ └────┬────┘ └────┬────┘ └────┬────┘                  │  │ │
│  │  │      │           │           │                        │  │ │
│  │  │  ┌───▼───────────▼───────────▼───┐                   │  │ │
│  │  │  │        PVCs (proxmox-csi)     │                   │  │ │
│  │  │  └───────────────────────────────┘                   │  │ │
│  │  └──────────────────────────────────────────────────────┘  │ │
│  │                              │                              │ │
│  │  Services: <cluster>-rw, <cluster>-ro, <cluster>-r         │ │
│  │  Secrets: <cluster>-superuser, <cluster>-app               │ │
│  └────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

### Service Types

| Service | DNS Name | Purpose |
|---------|----------|---------|
| Read-Write | `<cluster>-rw` | Primary only (writes) |
| Read-Only | `<cluster>-ro` | Replicas only (reads) |
| Read | `<cluster>-r` | Any instance |

### Secrets Created

| Secret | Contents |
|--------|----------|
| `<cluster>-superuser` | PostgreSQL superuser credentials |
| `<cluster>-app` | Application user credentials |

## Prerequisites

### Cluster Requirements

1. **Talos Kubernetes cluster** (Phase 4 complete)
   ```bash
   kubectl get nodes
   ```

2. **Cilium CNI operational** (Phase 6 complete)
   ```bash
   kubectl get pods -n kube-system -l k8s-app=cilium
   ```

3. **Proxmox CSI driver installed** (Phase 7 complete)
   ```bash
   kubectl get storageclass proxmox-csi
   ```

### Tools Required

- kubectl CLI
- Helm 3.x (for manual installation)
- psql client (optional, for testing)

## Installation

### Manual Installation (Phase 11)

#### Step 1: Create Namespace

```bash
kubectl create namespace cnpg-system
```

#### Step 2: Add Helm Repository

```bash
helm repo add cnpg https://cloudnative-pg.github.io/charts
helm repo update
```

#### Step 3: Install Operator

```bash
cd infrastructure-k8s/cnpg

helm install cloudnative-pg cnpg/cloudnative-pg \
  --version 0.22.1 \
  --namespace cnpg-system \
  --values helm-values.yaml
```

#### Step 4: Verify Installation

```bash
# Check operator pod
kubectl get pods -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg

# Check CRDs
kubectl get crds | grep cnpg

# Expected CRDs:
# backups.postgresql.cnpg.io
# clusters.postgresql.cnpg.io
# poolers.postgresql.cnpg.io
# scheduledbackups.postgresql.cnpg.io
```

### Flux Installation (Phase 8 Integration)

For GitOps deployments, Flux automatically manages the operator:

```bash
# Flux reconciles from clusters/production/infrastructure/
# Resources:
# - cnpg-helmrepo.yaml
# - cnpg-helmrelease.yaml
# - cnpg.yaml

# Check Flux status
flux get helmrelease cloudnative-pg -n cnpg-system
flux get kustomization cloudnative-pg
```

### Verification

```bash
# Deploy sample cluster
kubectl apply -f infrastructure-k8s/cnpg/verification/namespace.yaml
kubectl apply -f infrastructure-k8s/cnpg/verification/sample-cluster.yaml

# Watch cluster creation
kubectl get cluster -n cnpg-test -w

# Wait for Ready status (may take 2-5 minutes)
kubectl wait --for=condition=Ready cluster/sample-pg-cluster -n cnpg-test --timeout=300s
```

## Creating PostgreSQL Clusters

### Basic Cluster

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: my-database
  namespace: app
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

Apply:
```bash
kubectl apply -f cluster.yaml
```

### Cluster with Resources and Configuration

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: production-db
  namespace: app
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
      work_mem: "32MB"
      maintenance_work_mem: "128MB"
      log_min_duration_statement: "1000"
  
  monitoring:
    enablePodMonitor: true
```

### Bootstrap Options

**From scratch (initdb):**
```yaml
bootstrap:
  initdb:
    database: myapp
    owner: myuser
    encoding: UTF8
    localeCollate: en_US.UTF-8
    localeCType: en_US.UTF-8
```

**Clone from existing cluster:**
```yaml
bootstrap:
  pg_basebackup:
    source: source-cluster
    database: myapp
    owner: myuser
```

**Restore from backup:**
```yaml
bootstrap:
  recovery:
    source: backup-cluster
    recoveryTarget:
      targetTime: "2024-01-15 10:30:00"
```

## Connecting to Databases

### Port Forwarding (Development)

```bash
# Forward to primary (read-write)
kubectl port-forward -n app svc/my-database-rw 5432:5432

# Connect with psql
psql -h localhost -U postgres -d myapp
```

### In-Cluster Connection

Applications connect using service DNS:

```
# Read-write connection (primary)
postgresql://user:password@my-database-rw.app.svc.cluster.local:5432/myapp

# Read-only connection (replicas)
postgresql://user:password@my-database-ro.app.svc.cluster.local:5432/myapp
```

### Getting Credentials

```bash
# Superuser password
kubectl get secret my-database-superuser -n app -o jsonpath='{.data.password}' | base64 -d

# Application user password
kubectl get secret my-database-app -n app -o jsonpath='{.data.password}' | base64 -d

# Full connection URI
kubectl get secret my-database-app -n app -o jsonpath='{.data.uri}' | base64 -d
```

### Connection Pooling (PgBouncer)

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Pooler
metadata:
  name: my-database-pooler
  namespace: app
spec:
  cluster:
    name: my-database
  instances: 2
  type: rw
  pgbouncer:
    poolMode: transaction
    parameters:
      max_client_conn: "1000"
      default_pool_size: "20"
```

## Backup and Recovery

### Configure S3 Credentials

```bash
# Create S3 credentials secret
kubectl create secret generic s3-creds \
  --from-literal=ACCESS_KEY_ID=<your-access-key> \
  --from-literal=ACCESS_SECRET_KEY=<your-secret-key> \
  -n app
```

### Cluster with Backup

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: backed-up-db
  namespace: app
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 20Gi
  
  backup:
    barmanObjectStore:
      destinationPath: s3://my-bucket/postgres-backups
      endpointURL: https://s3.waddle.social  # For MinIO
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
  namespace: app
spec:
  schedule: "0 2 * * *"  # Daily at 2 AM
  backupOwnerReference: self
  cluster:
    name: backed-up-db
```

### On-Demand Backup

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Backup
metadata:
  name: manual-backup
  namespace: app
spec:
  cluster:
    name: backed-up-db
```

### Point-in-Time Recovery

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: restored-db
  namespace: app
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 20Gi
  
  bootstrap:
    recovery:
      source: backed-up-db
      recoveryTarget:
        targetTime: "2024-01-15 10:30:00"
  
  externalClusters:
    - name: backed-up-db
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

### Enable Metrics

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
| `cnpg_pg_locks_count` | Lock counts |

### Manual Metrics Query

```bash
# Port-forward to PostgreSQL pod
kubectl port-forward -n app my-database-1 9187:9187

# Query metrics
curl http://localhost:9187/metrics | grep cnpg
```

### Grafana Dashboard

Import CloudNativePG dashboard: ID `20417`

## High Availability

### Topology

- **3 instances:** 1 primary + 2 replicas (recommended for HA)
- **Synchronous replication:** Ensures data durability
- **Automatic failover:** Operator promotes replica on primary failure

### Pod Anti-Affinity

```yaml
spec:
  affinity:
    enablePodAntiAffinity: true
    topologyKey: kubernetes.io/hostname
```

### Replication Configuration

```yaml
spec:
  minSyncReplicas: 1
  maxSyncReplicas: 2
```

### Test Failover

```bash
# Identify primary
kubectl get pods -n app -l role=primary

# Delete primary (simulates failure)
kubectl delete pod my-database-1 -n app

# Watch failover
kubectl get pods -n app -w

# Verify new primary elected
kubectl get pods -n app -l role=primary
```

## Security

### Never Commit Credentials

PostgreSQL credentials are auto-generated in Kubernetes Secrets. Never store them in Git.

### Use Secret Management

For GitOps workflows:
- **sealed-secrets:** Encrypt secrets for Git storage
- **external-secrets:** Fetch from Vault, AWS Secrets Manager
- **sops:** Mozilla SOPS encryption

### Enable TLS

```yaml
spec:
  certificates:
    serverTLSSecret: pg-server-tls
    serverCASecret: pg-ca
```

### Network Policies

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: pg-access
  namespace: app
spec:
  podSelector:
    matchLabels:
      postgresql: my-database
  policyTypes:
    - Ingress
  ingress:
    - from:
        - podSelector:
            matchLabels:
              app: my-app
      ports:
        - port: 5432
```

### Credential Rotation

```bash
# Update password in Secret
kubectl patch secret my-database-superuser -n app \
  -p '{"data":{"password":"'$(echo -n "new-password" | base64)'"}}'
```

## Troubleshooting

### Cluster Not Ready

```bash
# Check cluster status
kubectl describe cluster my-database -n app

# Check operator logs
kubectl logs -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg

# Check pod events
kubectl describe pod my-database-1 -n app
```

### PVC Stuck in Pending

```bash
# Check PVC status
kubectl describe pvc my-database-1 -n app

# Check Proxmox CSI logs
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-controller
```

### Connection Issues

```bash
# Verify services exist
kubectl get svc -n app

# Check endpoints
kubectl get endpoints my-database-rw -n app

# Test connectivity
kubectl run -it --rm psql --image=postgres:16 --restart=Never -- \
  psql -h my-database-rw -U postgres -d myapp
```

### Replication Lag

```bash
# Check replication status
kubectl exec -it my-database-1 -n app -- \
  psql -U postgres -c "SELECT * FROM pg_stat_replication;"

# Check PostgreSQL logs
kubectl logs my-database-1 -n app -c postgres
```

### Backup Failures

```bash
# Check backup status
kubectl describe backup manual-backup -n app

# Check scheduled backup
kubectl describe scheduledbackup daily-backup -n app

# Verify S3 connectivity
kubectl exec -it my-database-1 -n app -- \
  barman-cloud-check-wal-archive --cloud-provider s3
```

## References

- [CloudNativePG Documentation](https://cloudnative-pg.io/documentation/)
- [CloudNativePG GitHub](https://github.com/cloudnative-pg/cloudnative-pg)
- [Helm Chart Repository](https://github.com/cloudnative-pg/charts)
- [Cluster API Reference](https://cloudnative-pg.io/documentation/current/cloudnative-pg.v1/)
- [Backup and Recovery Guide](https://cloudnative-pg.io/documentation/current/backup/)
- [Monitoring Guide](https://cloudnative-pg.io/documentation/current/monitoring/)
- [PostgreSQL Documentation](https://www.postgresql.org/docs/)
