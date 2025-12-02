# Waddle Infrastructure - Talos Kubernetes on Proxmox

A monorepo containing Infrastructure as Code (Terraform CDK) for provisioning Talos Kubernetes clusters on Proxmox 9.1, plus GitOps configurations (Flux) for cluster components and applications.

Supports flexible cluster topologies including:
- **3 control planes** (default) - HA cluster with workloads on control plane nodes
- **1 control plane + N workers** - Small cluster with dedicated worker nodes
- **3 control planes + N workers** - Production HA with dedicated workers

## Architecture

**Stack:** Proxmox → Talos → Kubernetes

- **CNI:** Cilium with Gateway API support
- **Storage:** Proxmox CSI
- **Ingress:** Gateway API
- **Database:** CloudNativePG, SpiceDB
- **Security:** cert-manager, Teleport
- **Observability:** LGTM (Loki, Grafana, Tempo, Mimir)

## Directory Structure

```
waddle-infra/
├── infrastructure/       # Terraform CDK (TypeScript) for Proxmox VMs and Talos cluster
├── clusters/             # Flux cluster-specific configurations (per-environment)
├── apps/                 # Application manifests (SpiceDB, etc.)
└── infrastructure-k8s/   # Cluster infrastructure components (Cilium, cert-manager, storage, observability)
```

## Prerequisites

- Node.js 20.9+ and npm
- Terraform CLI
- CDKTF CLI (`npm install -g cdktf-cli`)
- Proxmox 9.1 with API access
- Flux CLI
- kubectl (Kubernetes CLI)
- talosctl (Talos CLI)

### Terraform Providers

Provider bindings are generated via `cdktf get` (not npm packages). Versions are pinned in `infrastructure/cdktf.json`:

- **Proxmox:** bpg/proxmox ~> 0.78.0 → `.gen/providers/proxmox/`
- **Talos:** siderolabs/talos ~> 0.9.0 → `.gen/providers/talos/`

## Getting Started

1. **Clone repository**
   ```bash
   git clone <repository-url>
   cd waddle-infra
   ```

2. **Install dependencies**
   ```bash
   npm install
   ```

3. **Configure Proxmox credentials**
   
   Proxmox credentials can be provided via multiple methods:

   **Option A: Environment variables (for defaults)**
   ```bash
   cp infrastructure/.env.example infrastructure/.env
   # Edit with your values:
   PROXMOX_VE_ENDPOINT="https://proxmox.waddle.social:8006"
   PROXMOX_VE_API_TOKEN="root@pam!terraform=xxx"
   ```

   **Option B: Terraform variables at apply time**
   ```bash
   cdktf deploy -- -var="proxmox_endpoint=https://..." -var="proxmox_api_token=..."
   ```

   **Option C: terraform.tfvars file**
   Create `cdktf.out/stacks/waddle-infra/terraform.tfvars` after synth.

   To create an API token in Proxmox:
   1. Log into Proxmox web UI
   2. Navigate to: **Datacenter → Permissions → API Tokens → Add**
   3. Select a user and create a token (copy the secret - shown only once!)

4. **Configure Talos cluster settings** (for VM provisioning)

   Add to your `.env` file:
   ```bash
   PROXMOX_NODE_NAME=pve
   PROXMOX_STORAGE_ID=local-lvm        # VM disk storage
   PROXMOX_IMAGE_STORAGE_ID=local      # ISO/image storage (must support 'iso' content)
   TALOS_CLUSTER_ENDPOINT=https://192.168.1.101:6443
   ```

   **Network configuration - choose one mode:**

   **Static IP mode** (recommended):
   ```bash
   TALOS_NODE_IP_PREFIX=192.168.1      # First 3 octets only (/24 networks)
   TALOS_NODE_IP_START=101
   TALOS_NODE_GATEWAY=192.168.1.1
   ```

   **DHCP mode** (simpler setup):
   ```bash
   # Leave TALOS_NODE_IP_PREFIX and TALOS_NODE_GATEWAY unset
   # Configure DHCP reservations after seeing VM MAC addresses in Proxmox
   ```

   **Worker nodes** (optional):
   ```bash
   TALOS_WORKER_COUNT=2                # Number of worker nodes (default: 0)
   TALOS_WORKER_CORES=2
   TALOS_WORKER_MEMORY=4096
   ```

   **Note:** The `TALOS_CLUSTER_ENDPOINT` should be the VIP or first control plane IP for Kubernetes API access.

5. **Synthesize Terraform configuration**
   ```bash
   npm run synth
   ```

6. **Deploy infrastructure**
   ```bash
   npm run deploy
   ```

7. **Verify VMs in Proxmox web UI**

   After deployment, Talos VMs will be created with:
   - Control planes: `{cluster-name}-cp-00`, `{cluster-name}-cp-01`, etc.
   - Workers (if configured): `{cluster-name}-w-00`, `{cluster-name}-w-01`, etc.
   - IPs: Sequential from `TALOS_NODE_IP_START` (static mode) or assigned by DHCP
   - Topology labels stored in VM description

8. **Access the Kubernetes cluster** (after Phase 4 bootstrap)
   
   Extract kubeconfig from Terraform outputs:
   ```bash
   cd infrastructure
   cdktf output -raw kubeconfig_raw | base64 -d > ~/.kube/config
   ```
   
   Verify cluster access:
   ```bash
   kubectl get nodes
   kubectl get pods -A
   ```
   
   **Note**: After Phase 6, Cilium CNI enables pod networking. Verify with `kubectl get pods -A`.
   
   Extract talosconfig for Talos API access:
   ```bash
   cdktf output -raw talosconfig_raw | base64 -d > ~/.talos/config
   export TALOSCONFIG=~/.talos/config
   talosctl -n <node-ip> version
   ```

9. **Initialize Git repository** (after cluster is ready - Phase 6+)
   ```bash
   git init
   git add .
   git commit -m "Initial commit"
   git remote add origin <your-repo-url>
   git push -u origin main
   ```

10. **Bootstrap Flux** (after Git repository is pushed)
    ```bash
    export GITHUB_TOKEN=<your-github-pat>
    flux bootstrap github \
      --owner=<github-org-or-username> \
      --repository=waddle-infra \
      --path=clusters/production \
      --personal
    ```

11. **Verify Flux installation**
    ```bash
    flux check
    flux get kustomizations
    flux get helmreleases -A
    ```

## Development Workflow

### Infrastructure Changes
1. Modify TypeScript in `infrastructure/`
2. Run `npm run synth` to generate Terraform config
3. Run `npm run deploy` to apply changes

### Cluster Configuration Changes (GitOps)
1. Edit YAML in `clusters/`, `apps/`, or `infrastructure-k8s/`
2. Test locally: `kustomize build clusters/production/infrastructure/`
3. Commit and push to Git
4. Flux auto-reconciles within 1-10 minutes (depending on interval)
5. Force immediate reconciliation: `flux reconcile kustomization <name> --with-source`
6. View status: `flux get kustomizations`

## Security Notes

- **Never commit:** secrets, kubeconfigs, `.tfstate` files, or `.env` files
- **Access:** Use Teleport for all cluster and Proxmox access
- **Network Policies:** Zero-trust networking with default-deny policies (Phase 14)
- **Alerting:** Comprehensive alerts for infrastructure, applications, and security events (Phase 15)
- **Secrets Management:** Configure sealed-secrets or external-secrets (to be set up in later phases)
- **GitOps Security:**
  - Git repository access control (use fine-grained GitHub tokens)
  - Flux RBAC limits what controllers can modify
  - Audit trail via Git commit history
  - Use SOPS or sealed-secrets for encrypted secrets in Git

See `docs/security-hardening.md` for comprehensive security controls and recommendations.

## Implementation Progress

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | Monorepo initialization | ✅ Complete |
| 2 | Provider configuration (Proxmox, Talos) | ✅ Complete |
| 3 | Talos VM provisioning | ✅ Complete |
| 4 | Talos cluster bootstrap | ✅ Complete |
| 5 | Teleport secure access | ✅ Complete |
| 6 | Cilium CNI with Gateway API | ✅ Complete |
| 7 | Proxmox CSI driver for persistent storage | ✅ Complete |
| 8 | Flux GitOps setup | ✅ Complete |
| 9 | cert-manager with Let's Encrypt and Cloudflare DNS01 | ✅ Complete |
| 10 | Gateway API with TLS | ✅ Complete |
| 11 | CloudNativePG for PostgreSQL databases | ✅ Complete |
| 12 | SpiceDB authorization service | ✅ Complete |
| 13 | Observability stack (LGTM) | ✅ Complete |
| 14 | Network Policies and Security Hardening | ✅ Complete |
| 15 | Alerting (Alertmanager, PrometheusRules) | ✅ Complete |
| 16 | Operational Runbooks and DR Testing | ✅ Complete |

## Accessing Infrastructure

All infrastructure access is managed through Teleport for security and audit compliance.

**Install Teleport client:**
```bash
# macOS
brew install teleport

# Linux
curl https://get.gravitational.com/teleport-v17.0.0-linux-amd64-bin.tar.gz | tar -xz
sudo ./teleport/install
```

**Login:**
```bash
tsh login --proxy=teleport.waddle.social:443 --user=<your-username>
```

**Access Proxmox:**
```bash
# SSH to Proxmox host
tsh ssh root@pve

# Access Proxmox web UI
tsh apps login proxmox-web
tsh apps open proxmox-web
```

**Access Kubernetes:**
```bash
tsh kube login waddle-cluster
kubectl get nodes
```

See `docs/teleport-setup.md` for detailed access procedures and team onboarding.

## Cilium CNI

Cilium is installed as the Container Network Interface (CNI) for Kubernetes networking.

**Verify Cilium installation:**
```bash
kubectl get pods -n kube-system -l k8s-app=cilium
cilium status --wait
```

**Access Hubble UI (network observability):**
```bash
kubectl port-forward -n kube-system svc/hubble-ui 12000:80
# Open http://localhost:12000 in browser
```

See `infrastructure-k8s/cilium/README.md` for detailed configuration and troubleshooting.

## Persistent Storage

Proxmox CSI driver provides persistent storage for Kubernetes workloads using Proxmox storage backends.

**Verify CSI driver installation:**
```bash
# Check CSI driver pods
kubectl get pods -n csi-proxmox

# Check StorageClass
kubectl get storageclass proxmox-csi

# Test PVC creation
kubectl apply -f infrastructure-k8s/storage/verification/test-pvc.yaml
kubectl get pvc -n csi-test
```

See `infrastructure-k8s/storage/README.md` for configuration, troubleshooting, and usage examples.

## TLS Certificate Management

cert-manager provides automated TLS certificate issuance and renewal using Let's Encrypt ACME protocol with Cloudflare DNS01 challenge solver.

**Verify cert-manager installation:**
```bash
# Check cert-manager pods
kubectl get pods -n cert-manager

# Check ClusterIssuers
kubectl get clusterissuer

# Check certificate status
kubectl get certificate -A
```

**Request a certificate:**
```bash
# Create Certificate resource
kubectl apply -f - <<EOF
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: example-com-tls
  namespace: default
spec:
  secretName: example-com-tls
  issuerRef:
    name: letsencrypt-production
    kind: ClusterIssuer
  dnsNames:
    - waddle.social
    - www.waddle.social
EOF

# Check certificate status
kubectl describe certificate example-com-tls
```

See `infrastructure-k8s/cert-manager/README.md` for detailed configuration, troubleshooting, and usage examples.

## Gateway API Ingress

Gateway API provides HTTP/HTTPS ingress for external traffic with TLS termination using Cilium GatewayClass and cert-manager for automated certificate management.

**Verify Gateway installation:**
```bash
# Check Gateway status
kubectl get gateway -n gateway-ingress

# Check HTTPRoutes
kubectl get httproute -A

# Detailed Gateway status
kubectl describe gateway gateway -n gateway-ingress
```

**Get LoadBalancer IP:**
```bash
kubectl get gateway gateway -n gateway-ingress -o jsonpath='{.status.addresses[0].value}'
```

**Test external access:**
```bash
# After DNS is configured
curl -v https://waddle.social

# Or with Host header
export GATEWAY_IP=$(kubectl get gateway gateway -n gateway-ingress -o jsonpath='{.status.addresses[0].value}')
curl -k -H "Host: waddle.social" https://$GATEWAY_IP/
```

**Check TLS certificate:**
```bash
# Verify certificate is ready
kubectl get certificate -n gateway-ingress

# View certificate details
kubectl get secret gateway-tls -n gateway-ingress -o jsonpath='{.data.tls\.crt}' | base64 -d | openssl x509 -noout -text | head -20
```

See `infrastructure-k8s/gateway/README.md` and `docs/gateway-api-setup.md` for detailed configuration, DNS setup, and troubleshooting.

## PostgreSQL Databases (CloudNativePG)

CloudNativePG operator manages PostgreSQL clusters on Kubernetes with automated high availability, backup, and monitoring.

**Key Features:**
- **Operator Namespace:** `cnpg-system`
- **Storage:** Uses Proxmox CSI StorageClass `proxmox-csi`
- **High Availability:** Primary + replicas with automatic failover
- **Monitoring:** Prometheus metrics on port 9187

**Verify operator installation:**
```bash
# Check operator pod
kubectl get pods -n cnpg-system

# Check CRDs
kubectl get crds | grep cnpg

# List PostgreSQL clusters
kubectl get cluster -A
```

**Create a PostgreSQL cluster:**
```bash
# Deploy sample cluster (3 instances)
kubectl apply -f infrastructure-k8s/cnpg/verification/namespace.yaml
kubectl apply -f infrastructure-k8s/cnpg/verification/sample-cluster.yaml

# Check cluster status
kubectl get cluster -n cnpg-test

# Check pods (1 primary + 2 replicas)
kubectl get pods -n cnpg-test

# Get connection credentials
kubectl get secret sample-pg-cluster-superuser -n cnpg-test -o jsonpath='{.data.password}' | base64 -d
```

**Connect to PostgreSQL:**
```bash
# Port-forward to primary pod
kubectl port-forward -n cnpg-test sample-pg-cluster-1 5432:5432

# Connect with psql (in another terminal)
psql -h localhost -U postgres -d testdb
```

**Check cluster health:**
```bash
# Describe cluster resource
kubectl describe cluster sample-pg-cluster -n cnpg-test

# Check operator logs
kubectl logs -n cnpg-system -l app.kubernetes.io/name=cloudnative-pg
```

See `infrastructure-k8s/cnpg/README.md` and `docs/cloudnativepg-setup.md` for detailed configuration, backup setup, and troubleshooting.

## SpiceDB Authorization

SpiceDB is a Google Zanzibar-inspired authorization system providing relationship-based access control (ReBAC) for fine-grained permissions.

**Key Features:**
- **Relationship-Based Access Control:** Permissions defined through relationships, not static roles
- **Consistency Guarantees:** Protection against the "new enemy problem"
- **Schema Language:** Declarative Zed schema for authorization models
- **gRPC API:** High-performance permission checks on port 50051

**Verify SpiceDB installation:**
```bash
# Check SpiceDB operator
kubectl get pods -n spicedb -l app.kubernetes.io/name=spicedb-operator

# Check SpiceDB pods
kubectl get pods -n spicedb -l app.kubernetes.io/name=spicedb

# Check PostgreSQL cluster
kubectl get cluster -n spicedb

# Check CRDs
kubectl get crds | grep authzed
```

**Access SpiceDB:**
```bash
# Port-forward to SpiceDB gRPC API
kubectl port-forward -n spicedb svc/spicedb 50051:50051

# Get preshared key for authentication
kubectl get secret spicedb-config -n spicedb -o jsonpath='{.data.preshared_key}' | base64 -d

# Configure zed CLI
zed context set local localhost:50051 "<preshared_key>" --insecure
```

**Apply a schema:**
```bash
# Read current schema
zed schema read

# Write schema from file
zed schema write schema.zed
```

**Check permissions:**
```bash
# Create a relationship
zed relationship create document:readme viewer user:alice

# Check permission
zed permission check document:readme view user:alice
# Output: true
```

See `apps/spicedb/README.md` and `docs/spicedb-setup.md` for detailed configuration, schema management, and troubleshooting.

## Observability Stack (LGTM)

The LGTM observability stack provides unified logs, metrics, and traces for the entire cluster using Grafana, Loki, Tempo, and Mimir with OpenTelemetry Collector for telemetry collection.

**Components:**
- **Grafana** - Visualization and dashboards
- **Loki** - Log aggregation (LogQL)
- **Tempo** - Distributed tracing (TraceQL)
- **Mimir** - Metrics storage (PromQL, Prometheus-compatible)
- **OpenTelemetry Collector** - Unified telemetry collection
- **Prometheus Operator** - ServiceMonitor/PodMonitor CRDs

**Verify observability stack:**
```bash
# Check all components
kubectl get pods -n observability

# Check HelmReleases
flux get helmreleases -n observability

# Check ServiceMonitors
kubectl get servicemonitor -A

# Check PVCs
kubectl get pvc -n observability
```

**Access Grafana:**
```bash
# Port-forward to Grafana
kubectl port-forward -n observability svc/grafana 3000:80

# Open http://localhost:3000
# Default credentials: admin / <password-from-secret>

# Get admin password
kubectl get secret grafana-admin -n observability -o jsonpath='{.data.admin-password}' | base64 -d && echo
```

**Query logs (Loki):**
```logql
# In Grafana Explore, select Loki data source
{namespace="observability"}           # All logs from namespace
{} |= "error"                         # All error logs
{namespace="kube-system", app="cilium"}  # Cilium logs
```

**Query metrics (Mimir):**
```promql
# In Grafana Explore, select Mimir data source
sum(rate(container_cpu_usage_seconds_total[5m])) by (pod)  # CPU usage
kubelet_running_pods                                        # Running pods per node
cilium_drop_count_total                                     # Cilium packet drops
```

**Query traces (Tempo):**
```traceql
# In Grafana Explore, select Tempo data source
{}                           # All traces
{ status = error }           # Error traces
{ duration > 1s }            # Slow traces
```

**Pre-installed Dashboards:**
- Kubernetes Cluster Overview
- Cilium Network Observability
- Talos Node Metrics
- CloudNativePG PostgreSQL

See `infrastructure-k8s/observability/README.md` and `docs/observability-setup.md` for detailed configuration, querying, and troubleshooting.

## Alerting and Monitoring

Alertmanager routes alerts to notification channels (Slack, email, PagerDuty) based on severity and namespace.

**Access Alertmanager:**
```bash
kubectl port-forward -n observability svc/alertmanager 9093:9093
# Open http://localhost:9093
```

**View PrometheusRules:**
```bash
# List all alerting rules
kubectl get prometheusrules -n observability

# Describe specific rule
kubectl describe prometheusrule infrastructure-alerts -n observability
```

**Silence alerts:**
```bash
# Via Alertmanager UI or amtool CLI
amtool silence add alertname=NodeMemoryPressure --duration=2h
```

**Alert categories:**
- **Infrastructure**: Node health, pod crashes, storage issues
- **Cilium**: CNI and network policy alerts
- **cert-manager**: Certificate lifecycle alerts
- **CloudNativePG**: PostgreSQL database alerts
- **Flux**: GitOps reconciliation alerts

See `infrastructure-k8s/observability/alerting/README.md` for configuration details.

## Operational Runbooks

Step-by-step procedures for responding to alerts and common operational scenarios.

**Available Runbooks:**
- Node failures: `docs/runbooks/node-down.md`
- Pod crashes: `docs/runbooks/pod-crashlooping.md`
- Certificate expiration: `docs/runbooks/certificate-expiring.md`
- Database outages: `docs/runbooks/postgresql-down.md`

**On-call response times:**
| Severity | Response Time |
|----------|---------------|
| Critical | < 15 minutes |
| Warning | < 1 hour |
| Info | Next business day |

See `docs/runbooks/README.md` for complete list and procedures.

## Flux GitOps

Flux is installed for GitOps-based cluster management. All infrastructure and application configurations are reconciled from this Git repository.

**Bootstrap Flux (first-time setup):**
```bash
# Export GitHub token
export GITHUB_TOKEN=<your-github-pat>

# Bootstrap Flux
flux bootstrap github \
  --owner=<github-org-or-username> \
  --repository=waddle-infra \
  --path=clusters/production \
  --personal
```

**Directory Structure:**
- `clusters/production/` - Flux Kustomizations for production environment
- `infrastructure-k8s/` - Reusable infrastructure component manifests
- `apps/` - Application manifests

**Common Commands:**
```bash
# Check Flux status
flux get all

# View Kustomization status
flux get kustomizations

# View HelmRelease status
flux get helmreleases -A

# Force reconciliation
flux reconcile kustomization infrastructure --with-source

# View error logs
flux logs --level=error --all-namespaces
```

**GitOps Workflow:**
1. Edit YAML in `clusters/`, `apps/`, or `infrastructure-k8s/`
2. Commit and push to Git
3. Flux auto-reconciles within 1-10 minutes (depending on interval)
4. Force immediate reconciliation: `flux reconcile kustomization <name>`

See `docs/flux-workflow.md` for detailed workflow documentation.

## Next Steps

1. **Initialize Git repository**: Run `git init` and push to GitHub/GitLab
2. **Bootstrap Flux**: See [Flux GitOps](#flux-gitops) section above
3. **Create Grafana admin secret**: Before observability stack deploys
   ```bash
   kubectl create namespace observability
   kubectl create secret generic grafana-admin \
     --from-literal=admin-user=admin \
     --from-literal=admin-password=$(openssl rand -base64 32) \
     -n observability
   ```
4. **Create Alertmanager config secret**: For notification channels
   ```bash
   # See docs/observability-setup.md for configuration
   kubectl create secret generic alertmanager-config \
     --from-file=alertmanager.yaml \
     -n observability
   ```
5. **Schedule DR testing**: See `docs/disaster-recovery-testing.md` for test procedures
6. **Phase 17+:** PodSecurityStandards, ResourceQuotas, Image Scanning
7. **Phase 18+:** Deploy custom applications
