# SpiceDB Setup Guide

This guide provides comprehensive instructions for deploying and configuring SpiceDB, a Google Zanzibar-inspired authorization system for relationship-based access control on Kubernetes.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Schema Management](#schema-management)
- [Connecting to SpiceDB](#connecting-to-spicedb)
- [Usage Examples](#usage-examples)
- [Security](#security)
- [Monitoring](#monitoring)
- [Troubleshooting](#troubleshooting)
- [References](#references)

## Overview

### What is SpiceDB?

SpiceDB is a database for storing, computing, and validating fine-grained permissions. Inspired by Google's Zanzibar paper, it provides:

- **Relationship-Based Access Control (ReBAC):** Permissions defined through relationships, not static roles
- **Consistency Guarantees:** Protection against the "new enemy problem" with causal consistency
- **Schema Language:** Declarative schema (Zed) for defining authorization models
- **High Performance:** Designed for low-latency permission checks at scale
- **gRPC API:** Native gRPC interface (port 50051) with optional HTTP gateway

### Key Features

| Feature | Description |
|---------|-------------|
| Relationships | Store subject-object relationships (e.g., user:alice is viewer of document:readme) |
| Computed Permissions | Permissions computed from relationships at query time |
| Schema Versioning | Schema migrations with backward compatibility |
| Watch API | Real-time notifications of permission changes |
| Bulk Operations | Efficient bulk permission checks |
| PostgreSQL Datastore | Production-ready persistence with CloudNativePG |

### Version Information

- **SpiceDB Version:** v1.35.0
- **Operator Chart Version:** 2.2.0
- **PostgreSQL Datastore:** CloudNativePG cluster (3 instances)

## Architecture

### Component Overview

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
│  │  │  - Watches authzed.com/v1alpha1 resources             │  │ │
│  │  └──────────────────────┬───────────────────────────────┘  │ │
│  │                         │                                   │ │
│  │                         │ Manages                           │ │
│  │                         ▼                                   │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │  ┌─────────┐  ┌─────────┐  ┌─────────┐              │  │ │
│  │  │  │SpiceDB  │  │SpiceDB  │  │SpiceDB  │  Pods        │  │ │
│  │  │  │ Pod 1   │  │ Pod 2   │  │ Pod 3   │  (3 replicas)│  │ │
│  │  │  │ :50051  │  │ :50051  │  │ :50051  │  gRPC API    │  │ │
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
│  │  │  │Primary  │  │Replica  │  │Replica  │  3 instances  │  │ │
│  │  │  │ (rw)    │──│ (ro)    │──│ (ro)    │  20Gi storage │  │ │
│  │  │  └─────────┘  └─────────┘  └─────────┘               │  │ │
│  │  │                                                       │  │ │
│  │  │  Services: spicedb-postgres-rw, -ro, -r              │  │ │
│  │  └──────────────────────────────────────────────────────┘  │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  Client Access:                                                  │
│    kubectl port-forward -n spicedb svc/spicedb 50051:50051      │
│    In-cluster: spicedb.spicedb.svc.cluster.local:50051          │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Data Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                     Permission Check Flow                        │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. Application sends permission check request                   │
│       ↓                                                          │
│  2. gRPC request to SpiceDB (port 50051)                        │
│     Request: "Can user:alice view document:readme?"              │
│       ↓                                                          │
│  3. SpiceDB queries PostgreSQL for relationships                 │
│     - Fetches relevant relationships from datastore              │
│     - Evaluates schema rules                                     │
│       ↓                                                          │
│  4. SpiceDB computes permission based on schema                  │
│     - Checks if user:alice has direct viewer relation            │
│     - Checks if user:alice has editor relation (includes view)   │
│       ↓                                                          │
│  5. SpiceDB returns result                                       │
│     Response: HAS_PERMISSION or NO_PERMISSION                    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Components Table

| Component | Description | Location |
|-----------|-------------|----------|
| SpiceDB Operator | Manages SpiceDBCluster CRDs | spicedb namespace |
| SpiceDB Pods | gRPC API servers (3 replicas) | spicedb namespace |
| PostgreSQL Cluster | Datastore (CloudNativePG, 3 instances) | spicedb namespace |
| spicedb-config Secret | Preshared key + datastore URI | spicedb namespace |

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

4. **CloudNativePG operator** (Phase 11 complete)
   ```bash
   kubectl get pods -n cnpg-system
   kubectl get crds | grep cnpg
   ```

### Tools Required

| Tool | Purpose | Installation |
|------|---------|--------------|
| kubectl | Kubernetes CLI | https://kubernetes.io/docs/tasks/tools/ |
| zed | SpiceDB CLI | `brew install authzed/tap/zed` |
| grpcurl | gRPC testing | `brew install grpcurl` |
| Helm 3.x | Chart installation | `brew install helm` |

## Installation

### Manual Installation (Phase 12)

#### Step 1: Create Namespace

```bash
kubectl apply -f apps/spicedb/namespace.yaml
```

#### Step 2: Add Helm Repository

```bash
helm repo add spicedb https://bushelpowered.github.io/spicedb-operator-chart/
helm repo update
```

#### Step 3: Install SpiceDB Operator

```bash
cd apps/spicedb

helm install spicedb-operator spicedb/spicedb-operator \
  --version 2.2.0 \
  --namespace spicedb \
  --values helm-values.yaml
```

#### Step 4: Verify Operator Installation

```bash
# Check operator pod
kubectl get pods -n spicedb -l app.kubernetes.io/name=spicedb-operator

# Expected output:
# NAME                                READY   STATUS    RESTARTS   AGE
# spicedb-operator-xxxxxxxxx-xxxxx    1/1     Running   0          1m

# Check CRDs installed
kubectl get crds | grep authzed

# Expected CRDs:
# spicedbclusters.authzed.com
```

#### Step 5: Deploy PostgreSQL Cluster

```bash
kubectl apply -f apps/spicedb/postgres-cluster.yaml

# Watch cluster creation (takes 2-5 minutes)
kubectl get cluster -n spicedb -w

# Wait for Ready status
kubectl wait --for=condition=Ready cluster/spicedb-postgres -n spicedb --timeout=300s

# Check pods (1 primary + 2 replicas)
kubectl get pods -n spicedb -l postgresql=spicedb-postgres
```

#### Step 6: Create SpiceDB Secret

```bash
# Generate secure preshared key
PRESHARED_KEY=$(openssl rand -base64 32)
echo "Preshared Key: $PRESHARED_KEY"

# Get PostgreSQL password from CloudNativePG secret
PG_PASSWORD=$(kubectl get secret spicedb-postgres-app -n spicedb -o jsonpath='{.data.password}' | base64 -d)

# Construct datastore URI
DATASTORE_URI="postgres://spicedb:${PG_PASSWORD}@spicedb-postgres-rw.spicedb.svc.cluster.local:5432/spicedb?sslmode=disable"

# Create the secret
kubectl create secret generic spicedb-config -n spicedb \
  --from-literal=preshared_key="$PRESHARED_KEY" \
  --from-literal=datastore_uri="$DATASTORE_URI"

# Save the preshared key securely (you'll need it for client access)
echo "$PRESHARED_KEY" > ~/.spicedb-preshared-key
chmod 600 ~/.spicedb-preshared-key
```

#### Step 7: Deploy SpiceDB Cluster

```bash
kubectl apply -f apps/spicedb/spicedb-cluster.yaml

# Watch SpiceDB cluster creation
kubectl get spicedbcluster -n spicedb -w

# Check SpiceDB pods
kubectl get pods -n spicedb -l app.kubernetes.io/name=spicedb
```

### Flux Installation (GitOps)

For GitOps deployments, Flux automatically manages SpiceDB:

```bash
# Flux reconciles from clusters/production/apps/
# Resources:
# - spicedb-helmrepo.yaml
# - spicedb-helmrelease.yaml
# - spicedb.yaml

# Check Flux status
flux get helmrelease spicedb-operator -n spicedb
flux get kustomization spicedb

# Force reconciliation
flux reconcile kustomization spicedb --with-source
```

### Verification

```bash
# Check all SpiceDB components
kubectl get all -n spicedb

# Expected resources:
# - spicedb-operator deployment
# - spicedb StatefulSet (3 pods)
# - spicedb-postgres-1, -2, -3 pods
# - Services: spicedb, spicedb-postgres-rw, etc.
```

## Schema Management

### Install zed CLI

```bash
# macOS
brew install authzed/tap/zed

# Linux
curl -sL https://github.com/authzed/zed/releases/latest/download/zed_linux_amd64 -o zed
chmod +x zed
sudo mv zed /usr/local/bin/

# Verify installation
zed version
```

### Configure zed Context

```bash
# Port-forward to SpiceDB (in a separate terminal)
kubectl port-forward -n spicedb svc/spicedb 50051:50051

# Get preshared key
PRESHARED_KEY=$(kubectl get secret spicedb-config -n spicedb -o jsonpath='{.data.preshared_key}' | base64 -d)

# Set up zed context
zed context set local localhost:50051 "$PRESHARED_KEY" --insecure

# Verify connection
zed schema read
```

### Schema Definition Syntax

SpiceDB uses the Zed schema language to define authorization models:

```zed
// Basic type definition (no relations)
definition user {}

// Type with relations and permissions
definition document {
    // Relations define relationships
    relation owner: user
    relation editor: user
    relation viewer: user

    // Permissions are computed from relations
    permission delete = owner
    permission edit = owner + editor
    permission view = owner + editor + viewer
}

// Type with hierarchical relations
definition folder {
    relation parent: folder
    relation owner: user
    relation viewer: user

    permission view = viewer + owner + parent->view
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

### Schema Versioning

SpiceDB tracks schema versions for safety:

```bash
# Check current schema version
zed schema read | head -5

# Schema changes are validated for backward compatibility
# Breaking changes (removing permissions) require careful migration
```

### Migration Patterns

For production schema changes:

1. **Add new permissions/relations first** (non-breaking)
2. **Migrate application code** to use new schema
3. **Remove old permissions/relations** (breaking)

## Connecting to SpiceDB

### Port Forwarding (Development)

```bash
# Forward to SpiceDB gRPC port
kubectl port-forward -n spicedb svc/spicedb 50051:50051

# Now connect via localhost:50051
```

### In-Cluster Connection

Applications connect using service DNS:

```
# gRPC endpoint
spicedb.spicedb.svc.cluster.local:50051
```

### Client Library Examples

**Go Client:**
```go
import (
    "github.com/authzed/authzed-go/v1"
    "github.com/authzed/grpcutil"
)

client, err := authzed.NewClient(
    "spicedb.spicedb.svc.cluster.local:50051",
    grpcutil.WithInsecureBearerToken("your-preshared-key"),
    grpc.WithInsecure(),
)
```

**Python Client:**
```python
from authzed.api.v1 import Client
from grpcutil import insecure_bearer_token_credentials

client = Client(
    "spicedb.spicedb.svc.cluster.local:50051",
    insecure_bearer_token_credentials("your-preshared-key"),
)
```

**Node.js Client:**
```javascript
const { v1 } = require("@authzed/authzed-node");

const client = v1.NewClient(
  "your-preshared-key",
  "spicedb.spicedb.svc.cluster.local:50051",
  v1.ClientSecurity.INSECURE_LOCALHOST_ALLOWED
);
```

### Authentication

All requests must include the preshared key:

```bash
# Using zed CLI (configured via context)
zed permission check document:readme view user:alice

# Using grpcurl
grpcurl -plaintext \
  -H "authorization: Bearer <preshared_key>" \
  localhost:50051 \
  authzed.api.v1.PermissionsService/CheckPermission
```

## Usage Examples

### Creating Relationships

```bash
# Add user as document viewer
zed relationship create document:readme viewer user:alice

# Add user as document editor
zed relationship create document:readme editor user:bob

# Add user as document owner
zed relationship create document:readme owner user:charlie
```

### Checking Permissions

```bash
# Check if alice can view (she's a viewer)
zed permission check document:readme view user:alice
# Output: true

# Check if alice can edit (viewers can't edit)
zed permission check document:readme edit user:alice
# Output: false

# Check if bob can view (editors can view)
zed permission check document:readme view user:bob
# Output: true

# Check if charlie can delete (owners can delete)
zed permission check document:readme delete user:charlie
# Output: true
```

### Listing Relationships

```bash
# List all relationships for a document
zed relationship read document:readme

# List specific relation type
zed relationship read document:readme --relation viewer

# List all documents a user can access
zed permission lookup-resources document view user:alice
```

### Deleting Relationships

```bash
# Remove viewer relationship
zed relationship delete document:readme viewer user:alice

# Bulk delete (filter by subject)
zed relationship delete document:readme viewer user:*
```

### Bulk Operations

```bash
# Write multiple relationships
zed relationship bulk-write << EOF
document:doc1 viewer user:alice
document:doc1 editor user:bob
document:doc2 owner user:charlie
EOF

# Check multiple permissions
zed permission bulk-check << EOF
document:doc1 view user:alice
document:doc1 edit user:bob
document:doc2 delete user:charlie
EOF
```

## Security

### Preshared Key Management

- **Never commit** preshared keys to Git
- **Rotate periodically** (quarterly recommended)
- **Use strong keys:** `openssl rand -base64 32`
- **Store securely:** Use sealed-secrets or external-secrets for GitOps

### Secret Rotation

```bash
# Generate new preshared key
NEW_KEY=$(openssl rand -base64 32)

# Update secret
kubectl patch secret spicedb-config -n spicedb \
  -p '{"stringData":{"preshared_key":"'$NEW_KEY'"}}'

# Restart SpiceDB pods to pick up new key
kubectl rollout restart deployment/spicedb -n spicedb

# Update all clients with new key
```

### Network Policies

Restrict SpiceDB access to authorized namespaces:

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: spicedb-access
  namespace: spicedb
spec:
  podSelector:
    matchLabels:
      app.kubernetes.io/name: spicedb
  policyTypes:
    - Ingress
  ingress:
    - from:
        - namespaceSelector:
            matchLabels:
              spicedb-access: "true"
      ports:
        - port: 50051
```

### TLS Configuration

For production, enable TLS (future enhancement):

1. Create TLS certificate via cert-manager
2. Configure SpiceDB to use TLS
3. Update clients to use secure connection

### RBAC

Limit who can manage SpiceDB resources:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: spicedb-admin
  namespace: spicedb
rules:
  - apiGroups: ["authzed.com"]
    resources: ["spicedbclusters"]
    verbs: ["get", "list", "watch", "create", "update", "delete"]
```

## Monitoring

### Metrics Endpoints

SpiceDB exposes Prometheus metrics:

```bash
# Port-forward to metrics port (if exposed)
kubectl port-forward -n spicedb pod/spicedb-0 9090:9090

# Query metrics
curl http://localhost:9090/metrics | grep spicedb
```

### Key Metrics

| Metric | Description |
|--------|-------------|
| `spicedb_dispatch_count` | Number of dispatch operations |
| `spicedb_check_count` | Number of permission checks |
| `spicedb_check_duration` | Permission check latency |
| `spicedb_relationship_count` | Number of stored relationships |

### Prometheus Integration (Phase 13)

After installing Prometheus Operator:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: spicedb
  namespace: spicedb
spec:
  selector:
    matchLabels:
      app.kubernetes.io/name: spicedb
  podMetricsEndpoints:
    - port: metrics
```

### PostgreSQL Monitoring

The PostgreSQL cluster includes built-in monitoring:

```bash
# Check PostgreSQL metrics
kubectl port-forward -n spicedb pod/spicedb-postgres-1 9187:9187
curl http://localhost:9187/metrics | grep cnpg
```

## Troubleshooting

### Operator Not Starting

```bash
# Check operator pod status
kubectl describe pod -n spicedb -l app.kubernetes.io/name=spicedb-operator

# Check operator logs
kubectl logs -n spicedb -l app.kubernetes.io/name=spicedb-operator

# Common causes:
# - RBAC issues
# - Resource limits too low
# - Control plane tolerations missing
```

### SpiceDB Pods CrashLoopBackOff

```bash
# Check pod events
kubectl describe pod -n spicedb -l app.kubernetes.io/name=spicedb

# Check pod logs
kubectl logs -n spicedb -l app.kubernetes.io/name=spicedb

# Common causes:
# - Invalid preshared key format
# - PostgreSQL connection failure
# - Invalid datastore URI syntax
# - Secret not found
```

### PostgreSQL Connection Failures

```bash
# Verify PostgreSQL cluster is ready
kubectl get cluster -n spicedb
# Status should be "Cluster in healthy state"

# Check PostgreSQL pods
kubectl get pods -n spicedb -l postgresql=spicedb-postgres

# Test connection manually
kubectl exec -it spicedb-postgres-1 -n spicedb -- \
  psql -U spicedb -d spicedb -c "SELECT 1"

# Check datastore URI format
# Correct: postgres://spicedb:password@spicedb-postgres-rw.spicedb.svc.cluster.local:5432/spicedb?sslmode=disable
```

### Schema Validation Errors

```bash
# Validate schema locally
zed schema validate schema.zed

# Check for syntax errors
# - Missing definition keyword
# - Invalid relation type
# - Circular permission references

# Check SpiceDB logs for detailed errors
kubectl logs -n spicedb -l app.kubernetes.io/name=spicedb | grep -i schema
```

### Permission Check Returns Unexpected Results

```bash
# Read current schema
zed schema read

# List relationships for the object
zed relationship read document:readme

# Debug with verbose output
zed permission check document:readme view user:alice --explain

# Common issues:
# - Missing relationship
# - Wrong object/subject IDs
# - Schema doesn't include the permission path
```

### gRPC Connection Errors

```bash
# Test basic connectivity
grpcurl -plaintext localhost:50051 list

# If timeout:
# - Check port-forward is running
# - Verify SpiceDB pods are ready
# - Check network policies

# If authentication error:
# - Verify preshared key is correct
# - Check for whitespace in key
```

### Debugging Commands Reference

```bash
# Get all SpiceDB resources
kubectl get all -n spicedb

# Check events
kubectl get events -n spicedb --sort-by='.lastTimestamp'

# Operator logs
kubectl logs -n spicedb deployment/spicedb-operator -f

# SpiceDB logs
kubectl logs -n spicedb -l app.kubernetes.io/name=spicedb -f

# PostgreSQL logs
kubectl logs -n spicedb pod/spicedb-postgres-1 -c postgres

# Describe SpiceDBCluster
kubectl describe spicedbcluster spicedb -n spicedb
```

## References

- [SpiceDB Documentation](https://authzed.com/docs)
- [SpiceDB GitHub](https://github.com/authzed/spicedb)
- [SpiceDB Operator Chart](https://github.com/bushelpowered/spicedb-operator-chart)
- [Zed CLI Documentation](https://authzed.com/docs/reference/zed)
- [Schema Language Reference](https://authzed.com/docs/reference/schema-lang)
- [Google Zanzibar Paper](https://research.google/pubs/pub48190/)
- [Client Libraries](https://authzed.com/docs/reference/clients)
- [CloudNativePG Documentation](https://cloudnative-pg.io/documentation/)
