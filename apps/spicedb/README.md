# SpiceDB Authorization Service

This directory contains configuration for deploying SpiceDB, a Google Zanzibar-inspired authorization system providing relationship-based access control (ReBAC) for Kubernetes applications.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Components](#components)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Configuration](#configuration)
- [Schema Management](#schema-management)
- [Usage Examples](#usage-examples)
- [Verification](#verification)
- [Troubleshooting](#troubleshooting)
- [Files in This Directory](#files-in-this-directory)
- [References](#references)

## Overview

### What is SpiceDB?

SpiceDB is a database for storing, computing, and validating fine-grained permissions. Inspired by Google's Zanzibar paper, it provides:

- **Relationship-Based Access Control (ReBAC):** Define permissions through relationships, not static roles
- **Consistency Guarantees:** Protection against the "new enemy problem" with causal consistency
- **Schema Language:** Declarative schema for defining authorization models
- **High Performance:** Designed for low-latency permission checks at scale
- **gRPC API:** Native gRPC interface with optional HTTP gateway

### Key Features

| Feature | Description |
|---------|-------------|
| Relationships | Store subject-object relationships (e.g., user:alice is viewer of document:readme) |
| Computed Permissions | Permissions are computed from relationships at query time |
| Schema Versioning | Schema migrations with backward compatibility |
| Watch API | Real-time notifications of permission changes |
| Bulk Operations | Efficient bulk permission checks |

### Version Information

- **SpiceDB Version:** v1.35.0
- **Operator Chart Version:** 2.2.0
- **PostgreSQL Datastore:** CloudNativePG cluster

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     SpiceDB Architecture                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                    spicedb namespace                        │ │
│  │                                                             │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │              SpiceDB Operator                         │  │ │
│  │  │  - Manages SpiceDBCluster CRDs                        │  │ │
│  │  │  - Handles deployment and scaling                     │  │ │
│  │  └──────────────────────┬───────────────────────────────┘  │ │
│  │                         │                                   │ │
│  │                         │ Manages                           │ │
│  │                         ▼                                   │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │              SpiceDBCluster Resource                  │  │ │
│  │  │  - version: v1.35.0                                   │  │ │
│  │  │  - replicas: 3                                        │  │ │
│  │  │  - datastoreEngine: postgres                          │  │ │
│  │  └──────────────────────┬───────────────────────────────┘  │ │
│  │                         │                                   │ │
│  │                         │ Creates                           │ │
│  │                         ▼                                   │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │  ┌─────────┐  ┌─────────┐  ┌─────────┐              │  │ │
│  │  │  │SpiceDB  │  │SpiceDB  │  │SpiceDB  │  Pods        │  │ │
│  │  │  │ Pod 1   │  │ Pod 2   │  │ Pod 3   │  (3 replicas)│  │ │
│  │  │  │ :50051  │  │ :50051  │  │ :50051  │              │  │ │
│  │  │  └────┬────┘  └────┬────┘  └────┬────┘              │  │ │
│  │  │       │            │            │                    │  │ │
│  │  │       └────────────┼────────────┘                    │  │ │
│  │  │                    │                                 │  │ │
│  │  │  Service: spicedb  │ (ClusterIP, port 50051)         │  │ │
│  │  └────────────────────┼─────────────────────────────────┘  │ │
│  │                       │                                     │ │
│  │                       │ Connects to                         │ │
│  │                       ▼                                     │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │           PostgreSQL Cluster (CloudNativePG)          │  │ │
│  │  │  ┌─────────┐  ┌─────────┐  ┌─────────┐               │  │ │
│  │  │  │Primary  │  │Replica  │  │Replica  │               │  │ │
│  │  │  │ (rw)    │──│ (ro)    │──│ (ro)    │               │  │ │
│  │  │  └─────────┘  └─────────┘  └─────────┘               │  │ │
│  │  │                                                       │  │ │
│  │  │  Services: spicedb-postgres-rw, -ro, -r              │  │ │
│  │  └──────────────────────────────────────────────────────┘  │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  Client Access:                                                  │
│    - Port-forward: kubectl port-forward svc/spicedb 50051:50051 │
│    - In-cluster: spicedb.spicedb.svc.cluster.local:50051        │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Components

| Component | Description | Namespace |
|-----------|-------------|-----------|
| SpiceDB Operator | Manages SpiceDBCluster CRDs | spicedb |
| SpiceDB Pods | gRPC API servers (3 replicas) | spicedb |
| PostgreSQL Cluster | Datastore (CloudNativePG) | spicedb |
| spicedb-config Secret | Preshared key and datastore URI | spicedb |

## Prerequisites

Before installing SpiceDB, ensure:

1. **CloudNativePG operator** (Phase 11 complete)
   ```bash
   kubectl get pods -n cnpg-system
   ```

2. **Proxmox CSI driver** (Phase 7 complete)
   ```bash
   kubectl get storageclass proxmox-csi
   ```

3. **Cilium CNI** (Phase 6 complete)
   ```bash
   kubectl get pods -n kube-system -l k8s-app=cilium
   ```

### Tools Required

- kubectl CLI
- zed CLI (SpiceDB command-line tool)
- Helm 3.x (for manual installation)

## Installation

### Manual Installation

#### Step 1: Create Namespace

```bash
kubectl apply -f namespace.yaml
```

#### Step 2: Install SpiceDB Operator

```bash
helm repo add spicedb https://bushelpowered.github.io/spicedb-operator-chart/
helm repo update

helm install spicedb-operator spicedb/spicedb-operator \
  --version 2.2.0 \
  --namespace spicedb \
  --values helm-values.yaml
```

#### Step 3: Deploy PostgreSQL Cluster

```bash
kubectl apply -f postgres-cluster.yaml

# Wait for PostgreSQL to be ready
kubectl wait --for=condition=Ready cluster/spicedb-postgres -n spicedb --timeout=300s
```

#### Step 4: Create SpiceDB Secret

```bash
# Generate preshared key
PRESHARED_KEY=$(openssl rand -base64 32)

# Get PostgreSQL password
PG_PASSWORD=$(kubectl get secret spicedb-postgres-app -n spicedb -o jsonpath='{.data.password}' | base64 -d)

# Create secret
kubectl create secret generic spicedb-config -n spicedb \
  --from-literal=preshared_key="$PRESHARED_KEY" \
  --from-literal=datastore_uri="postgres://spicedb:${PG_PASSWORD}@spicedb-postgres-rw.spicedb.svc.cluster.local:5432/spicedb?sslmode=disable"
```

#### Step 5: Deploy SpiceDB Cluster

```bash
kubectl apply -f spicedb-cluster.yaml

# Check status
kubectl get spicedbcluster -n spicedb
kubectl get pods -n spicedb
```

### Flux GitOps Installation

Flux automatically manages SpiceDB deployment:

```bash
# Flux reconciles from clusters/production/apps/
# Resources:
# - spicedb-helmrepo.yaml
# - spicedb-helmrelease.yaml
# - spicedb.yaml

# Check Flux status
flux get helmrelease spicedb-operator -n spicedb
flux get kustomization spicedb
```

## Configuration

### Preshared Key Generation

```bash
# Generate a secure 32-byte base64-encoded key
openssl rand -base64 32

# Alternative using /dev/urandom
head -c 32 /dev/urandom | base64
```

### PostgreSQL Connection String

Format: `postgres://user:password@host:port/database?sslmode=disable`

```bash
# Get password from CloudNativePG secret
kubectl get secret spicedb-postgres-app -n spicedb -o jsonpath='{.data.password}' | base64 -d

# Full connection string
postgres://spicedb:<PASSWORD>@spicedb-postgres-rw.spicedb.svc.cluster.local:5432/spicedb?sslmode=disable
```

### Resource Limits

SpiceDB pods are managed by the operator. Configure resources in SpiceDBCluster spec if needed (not currently exposed in this basic configuration).

## Schema Management

### Install zed CLI

```bash
# macOS
brew install authzed/tap/zed

# Linux
curl -sL https://github.com/authzed/zed/releases/latest/download/zed_linux_amd64 -o zed
chmod +x zed && sudo mv zed /usr/local/bin/

# Verify installation
zed version
```

### Configure zed Context

```bash
# Port-forward to SpiceDB
kubectl port-forward -n spicedb svc/spicedb 50051:50051

# Get preshared key
PRESHARED_KEY=$(kubectl get secret spicedb-config -n spicedb -o jsonpath='{.data.preshared_key}' | base64 -d)

# Set context
zed context set local localhost:50051 "$PRESHARED_KEY" --insecure
```

### Schema Definition

SpiceDB uses the Zed schema language:

```zed
definition user {}

definition document {
    relation viewer: user
    relation editor: user

    permission view = viewer + editor
    permission edit = editor
}
```

### Apply Schema

```bash
# Write schema from file
zed schema write schema.zed

# Read current schema
zed schema read

# Validate schema without applying
zed schema validate schema.zed
```

## Usage Examples

### Creating Relationships

```bash
# Add user:alice as viewer of document:readme
zed relationship create document:readme viewer user:alice

# Add user:bob as editor of document:readme
zed relationship create document:readme editor user:bob
```

### Checking Permissions

```bash
# Check if alice can view
zed permission check document:readme view user:alice
# Output: true

# Check if alice can edit
zed permission check document:readme edit user:alice
# Output: false

# Check if bob can edit
zed permission check document:readme edit user:bob
# Output: true
```

### Listing Relationships

```bash
# List all relationships for a document
zed relationship read document:readme

# List all viewers
zed relationship read document:readme --relation viewer
```

### Deleting Relationships

```bash
# Remove alice as viewer
zed relationship delete document:readme viewer user:alice
```

## Verification

### Quick Test

```bash
# 1. Port-forward
kubectl port-forward -n spicedb svc/spicedb 50051:50051

# 2. Get preshared key and configure zed
PRESHARED_KEY=$(kubectl get secret spicedb-config -n spicedb -o jsonpath='{.data.preshared_key}' | base64 -d)
zed context set local localhost:50051 "$PRESHARED_KEY" --insecure

# 3. Apply test schema
kubectl get configmap spicedb-test-schema -n spicedb -o jsonpath='{.data.schema\.zed}' > /tmp/schema.zed
zed schema write /tmp/schema.zed

# 4. Create relationships
zed relationship create document:test viewer user:alice
zed relationship create document:test editor user:bob

# 5. Test permissions
zed permission check document:test view user:alice  # true
zed permission check document:test edit user:alice  # false
zed permission check document:test edit user:bob    # true
```

### In-Cluster Test

```bash
# Deploy test client
kubectl apply -f verification/test-client.yaml

# Exec into pod
kubectl exec -it spicedb-test-client -n spicedb -- /bin/sh

# Install grpcurl
apk add --no-cache curl
curl -sL https://github.com/fullstorydev/grpcurl/releases/download/v1.8.9/grpcurl_1.8.9_linux_x86_64.tar.gz | tar -xz
mv grpcurl /usr/local/bin/

# Test gRPC connectivity
grpcurl -plaintext spicedb.spicedb.svc.cluster.local:50051 list
```

## Troubleshooting

### Operator Not Starting

```bash
# Check operator pod
kubectl describe pod -n spicedb -l app.kubernetes.io/name=spicedb-operator

# Check operator logs
kubectl logs -n spicedb -l app.kubernetes.io/name=spicedb-operator
```

### SpiceDB Pods CrashLoopBackOff

```bash
# Check pod events
kubectl describe pod -n spicedb -l app.kubernetes.io/name=spicedb

# Check pod logs
kubectl logs -n spicedb -l app.kubernetes.io/name=spicedb

# Common causes:
# - Invalid preshared key
# - PostgreSQL connection failure
# - Invalid datastore URI
```

### PostgreSQL Connection Failures

```bash
# Verify PostgreSQL cluster is ready
kubectl get cluster -n spicedb

# Check PostgreSQL pods
kubectl get pods -n spicedb -l postgresql=spicedb-postgres

# Test connection from SpiceDB pod
kubectl exec -it spicedb-0 -n spicedb -- \
  psql "postgres://spicedb:xxx@spicedb-postgres-rw:5432/spicedb?sslmode=disable"
```

### Schema Validation Errors

```bash
# Validate schema locally
zed schema validate schema.zed

# Check SpiceDB logs for schema errors
kubectl logs -n spicedb -l app.kubernetes.io/name=spicedb | grep -i schema
```

### Permission Check Failures

```bash
# Enable debug logging
# Update spicedb-cluster.yaml: logLevel: debug

# Check current schema
zed schema read

# List existing relationships
zed relationship read <object_type>:<object_id>
```

## Files in This Directory

| File | Description |
|------|-------------|
| `README.md` | This documentation file |
| `namespace.yaml` | SpiceDB namespace definition |
| `kustomization.yaml` | Kustomize aggregation for all resources |
| `helm-values.yaml` | Helm values for SpiceDB operator |
| `postgres-cluster.yaml` | CloudNativePG Cluster for SpiceDB datastore |
| `spicedb-secret.example.yaml` | Secret template (preshared key + datastore URI) - NOT applied by Flux |
| `spicedb-cluster.yaml` | SpiceDBCluster custom resource |
| `verification/test-schema.yaml` | Sample schema for testing |
| `verification/test-client.yaml` | Test pod for in-cluster testing |

## References

- [SpiceDB Documentation](https://authzed.com/docs)
- [SpiceDB GitHub](https://github.com/authzed/spicedb)
- [SpiceDB Operator Chart](https://github.com/bushelpowered/spicedb-operator-chart)
- [Zed CLI Documentation](https://authzed.com/docs/reference/zed)
- [Schema Language Reference](https://authzed.com/docs/reference/schema-lang)
- [Google Zanzibar Paper](https://research.google/pubs/pub48190/)
- [CloudNativePG Documentation](https://cloudnative-pg.io/documentation/)
