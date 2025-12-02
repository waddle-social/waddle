# Application Manifests

This directory contains application manifests and configurations for the Kubernetes cluster. Each subdirectory represents an application or service deployed to the cluster.

## Table of Contents

- [Purpose](#purpose)
- [Directory Structure](#directory-structure)
- [Relationship to clusters/](#relationship-to-clusters)
- [Application Lifecycle](#application-lifecycle)
- [Dependencies](#dependencies)
- [Helm vs Raw Manifests](#helm-vs-raw-manifests)
- [Configuration Management](#configuration-management)
- [Planned Applications](#planned-applications)
- [Adding a New Application](#adding-a-new-application)

## Purpose

The `apps/` directory serves as the source of truth for application configurations. It contains:

- **Helm Values:** Configuration files for Helm-based deployments
- **Kustomize Bases:** Raw Kubernetes manifests organized for Kustomize
- **Application-Specific Resources:** ConfigMaps, Secrets templates, CRDs, etc.

**Separation of Concerns:**
- `apps/` → Application manifests and configurations (reusable across environments)
- `clusters/production/apps/` → Flux Kustomizations that reference these manifests
- `infrastructure-k8s/` → Infrastructure components (CNI, storage, cert-manager)

## Directory Structure

```
apps/
├── README.md                    # This file
├── .gitkeep                     # Ensures directory is tracked (remove when apps added)
├── spicedb/                     # SpiceDB authorization service (Phase 11)
│   ├── kustomization.yaml
│   ├── helm-values.yaml
│   ├── namespace.yaml
│   └── verification/
│       └── test-schema.yaml
├── observability/               # Observability stack (Phase 12)
│   ├── kustomization.yaml
│   ├── grafana/
│   ├── loki/
│   ├── tempo/
│   └── mimir/
└── <custom-app>/                # Custom applications (Phase 13+)
    ├── kustomization.yaml
    ├── deployment.yaml
    ├── service.yaml
    └── configmap.yaml
```

## Relationship to clusters/

The `clusters/production/apps/` directory contains Flux Kustomization resources that reference manifests in this directory:

```yaml
# clusters/production/apps/spicedb.yaml
apiVersion: kustomize.toolkit.fluxcd.io/v1
kind: Kustomization
metadata:
  name: spicedb
  namespace: flux-system
spec:
  sourceRef:
    kind: GitRepository
    name: flux-system
  path: ./apps/spicedb          # Points to apps/spicedb/
  interval: 10m
  dependsOn:
    - name: cloudnative-pg      # Requires database operator
```

**Flow:**
1. Developer edits files in `apps/<app-name>/`
2. Commits and pushes to Git
3. Flux Kustomization in `clusters/production/apps/` detects changes
4. Flux applies the updated manifests to the cluster

## Application Lifecycle

### Development → Staging → Production

For multi-environment deployments:

1. **Base configuration** in `apps/<app-name>/`
2. **Environment overlays** using Kustomize patches or separate values files:
   ```
   apps/<app-name>/
   ├── base/
   │   ├── kustomization.yaml
   │   └── deployment.yaml
   └── overlays/
       ├── staging/
       │   └── kustomization.yaml
       └── production/
           └── kustomization.yaml
   ```

3. **Flux Kustomizations** in each environment reference the appropriate overlay:
   ```yaml
   # clusters/staging/apps/my-app.yaml
   spec:
     path: ./apps/my-app/overlays/staging
   ```

### Version Promotion

Promote applications between environments:

1. Test in staging environment
2. Merge changes to main branch
3. Production Flux reconciles automatically
4. Rollback by reverting Git commits if needed

## Dependencies

Applications depend on infrastructure components to be ready:

| Dependency | Required By | Phase |
|------------|-------------|-------|
| Cilium CNI | All apps (networking) | 6 |
| Proxmox CSI | Apps with PVCs | 7 |
| cert-manager | Apps with TLS | 9 |
| CloudNativePG | Apps with PostgreSQL | 10 |
| Gateway API | Apps with ingress | 9 |

**Flux handles dependencies via `dependsOn`:**
```yaml
# clusters/production/apps/spicedb.yaml
spec:
  dependsOn:
    - name: infrastructure    # Wait for all infrastructure
    # Or specific dependencies:
    # - name: cloudnative-pg
    #   namespace: flux-system
```

## Helm vs Raw Manifests

### When to Use Helm

- Complex applications with many configurable parameters
- Third-party applications with official Helm charts
- Applications requiring conditional resources based on values
- Standardized deployment patterns

**Example:** SpiceDB, Grafana, PostgreSQL operators

### When to Use Raw Manifests

- Simple applications with few resources
- Custom applications specific to this cluster
- Applications requiring fine-grained control
- Learning/debugging deployments

**Example:** Simple microservices, test workloads

### Hybrid Approach

Use Helm for initial deployment, export manifests for customization:

```bash
helm template my-release my-chart/ --values values.yaml > manifests.yaml
```

## Configuration Management

### Environment Variables

Use ConfigMaps for non-sensitive configuration:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: app-config
data:
  DATABASE_HOST: "postgres.database.svc.cluster.local"
  LOG_LEVEL: "info"
```

### Secrets

**Never commit plain-text secrets to Git.**

Options for secret management:

1. **Sealed Secrets:** Encrypt secrets client-side
   ```bash
   kubeseal --format yaml < secret.yaml > sealed-secret.yaml
   ```

2. **External Secrets:** Fetch from Vault, AWS Secrets Manager
   ```yaml
   apiVersion: external-secrets.io/v1beta1
   kind: ExternalSecret
   spec:
     secretStoreRef:
       name: vault-backend
     target:
       name: app-secret
   ```

3. **SOPS:** Encrypt with age/gpg
   ```bash
   sops --encrypt secret.yaml > secret.enc.yaml
   ```

### Helm Values

Store Helm values in `helm-values.yaml`:

```yaml
# apps/spicedb/helm-values.yaml
replicaCount: 3
resources:
  requests:
    cpu: 100m
    memory: 256Mi
```

Reference in Flux HelmRelease:

```yaml
valuesFrom:
  - kind: ConfigMap
    name: spicedb-values
    valuesKey: values.yaml
```

## Planned Applications

| Application | Phase | Purpose | Dependencies |
|-------------|-------|---------|--------------|
| SpiceDB | 11 | Authorization service (Zanzibar) | CloudNativePG |
| Grafana | 12 | Metrics visualization | - |
| Loki | 12 | Log aggregation | Proxmox CSI |
| Tempo | 12 | Distributed tracing | Proxmox CSI |
| Mimir | 12 | Long-term metrics storage | Proxmox CSI |

## Adding a New Application

### Step 1: Create Application Directory

```bash
mkdir -p apps/<app-name>
```

### Step 2: Create kustomization.yaml

```yaml
# apps/<app-name>/kustomization.yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization

namespace: <app-namespace>

resources:
  - namespace.yaml
  - deployment.yaml
  - service.yaml
  - configmap.yaml
```

### Step 3: Create Application Manifests

```yaml
# apps/<app-name>/namespace.yaml
apiVersion: v1
kind: Namespace
metadata:
  name: <app-namespace>
```

```yaml
# apps/<app-name>/deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: <app-name>
spec:
  replicas: 1
  selector:
    matchLabels:
      app: <app-name>
  template:
    metadata:
      labels:
        app: <app-name>
    spec:
      containers:
        - name: <app-name>
          image: <image>:<tag>
```

### Step 4: Create Flux Kustomization

```yaml
# clusters/production/apps/<app-name>.yaml
apiVersion: kustomize.toolkit.fluxcd.io/v1
kind: Kustomization
metadata:
  name: <app-name>
  namespace: flux-system
spec:
  sourceRef:
    kind: GitRepository
    name: flux-system
  path: ./apps/<app-name>
  interval: 10m
  prune: true
  dependsOn:
    - name: infrastructure
```

### Step 5: Add to apps kustomization.yaml

```yaml
# clusters/production/apps/kustomization.yaml
resources:
  - <app-name>.yaml
```

### Step 6: Commit and Push

```bash
git add apps/<app-name> clusters/production/apps/
git commit -m "Add <app-name> application"
git push
```

### Step 7: Verify Deployment

```bash
flux get kustomization <app-name>
kubectl get pods -n <app-namespace>
```

## Files in This Directory

| File/Directory | Description |
|----------------|-------------|
| `README.md` | This documentation file |
| `.gitkeep` | Placeholder to track empty directory (remove when apps added) |

## References

- [Flux Multi-tenancy](https://fluxcd.io/flux/installation/configuration/multitenancy/)
- [Kustomize Documentation](https://kustomize.io/)
- [Helm Best Practices](https://helm.sh/docs/chart_best_practices/)
- [Sealed Secrets](https://github.com/bitnami-labs/sealed-secrets)
- [External Secrets Operator](https://external-secrets.io/)
