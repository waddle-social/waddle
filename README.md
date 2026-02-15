# waddle.social Infrastructure

OpenTofu IaC for the waddle.social platform. Provisions a 3-node Talos Linux Kubernetes cluster on a single Proxmox VE host (Scaleway Elastic Metal), with Teleport for zero-trust access and HAProxy for public ingress.

## Architecture Overview

```
Internet
  │
  ▼
HAProxy VM (public IP, vmbr0 + vmbr1)
  │  TCP/SNI routing on :443, no TLS termination
  │
  ├── teleport.waddle.social ──► Teleport VM (10.10.0.2:3080)
  ├── proxmox.waddle.social  ──► Teleport VM (10.10.0.2:3080) ──► Proxmox (10.10.0.1:8006)
  ├── *.apps.waddle.social   ──► Cilium Gateway VIP (10.10.0.30:443)
  └── :80                    ──► Cilium Gateway VIP (10.10.0.30:80)

                    vmbr1 (10.10.0.0/24)
                    ┌──────────────────────────────────────────┐
                    │                                          │
  Proxmox Host ─────┤ 10.10.0.1   (NAT gateway, iSCSI target) │
  HAProxy VM ───────┤ 10.10.0.3                                │
  Teleport VM ──────┤ 10.10.0.2   (internal only, no vmbr0)   │
  Talos CP1 ────────┤ 10.10.0.10                               │
  Talos CP2 ────────┤ 10.10.0.11                               │
  Talos CP3 ────────┤ 10.10.0.12                               │
  Talos VIP ────────┤ 10.10.0.20  (floating, K8s API)         │
  Cilium GW VIP ────┤ 10.10.0.30  (L2, public services)       │
                    └──────────────────────────────────────────┘
```

## Component Versions

| Component | Version |
|---|---|
| OpenTofu | >= 1.11.4 |
| Proxmox VE | 9.1 |
| bpg/proxmox provider | 0.95.0 |
| Talos Linux | v1.12.4 |
| Kubernetes | 1.35.1 |
| Cilium | 1.19.0 |
| democratic-csi | 0.15.1 (Helm) |
| Flux Operator | 0.40.0 |
| cert-manager | 1.19.3 |
| External DNS | 1.20.0 (Helm) |
| External Secrets Operator | 2.0.0 |
| 1Password Connect | 2.3.0 (Helm) |
| CloudNativePG | 1.28.1 |
| SpiceDB Operator | 1.22.0 |
| Teleport | 18.x (Community) |
| Debian | 12 (Bookworm) |

## Repository Layout

```
waddle-infra/
├── tofu/                                  # OpenTofu IaC (Phases 0-4)
│   ├── backend.tf                        # Scaleway S3 state backend
│   ├── versions.tf                       # OpenTofu + provider version pins
│   ├── variables.tf                      # All input variables
│   ├── terraform.tfvars.example          # Example variable values (no secrets)
│   ├── main.tf                           # Root module, wires all modules
│   └── modules/
│       ├── network/                      # vmbr1 bridge, pve-firewall, cluster firewall
│       ├── haproxy/                      # HAProxy VM, SNI routing config
│       ├── teleport/                     # Teleport VM, GitHub SSO, app access
│       └── talos-cluster/                # 3x Talos VMs from Image Factory
├── talos/                                 # Talos machine config (Phase 4)
│   ├── controlplane.yaml.tpl            # Talos machine config template
│   └── patches/                          # Per-node config patches
├── platform/                              # Flux GitOps manifests (Phase 6)
│   ├── clusters/scaleway/                # Cluster entry point for Flux
│   ├── infrastructure/                   # Operators and shared infra
│   │   ├── external-secrets-operator/    # ESO 2.0.0 HelmRelease
│   │   ├── onepassword-connect/          # 1Password Connect + ClusterSecretStore
│   │   ├── cert-manager/                 # cert-manager + ClusterIssuer (DNS-01)
│   │   ├── external-dns/                 # External DNS (Cloudflare)
│   │   ├── cloudnative-pg/               # CloudNativePG operator
│   │   ├── spicedb-operator/             # SpiceDB operator
│   │   └── cilium-gateway/               # Gateway API + wildcard TLS cert
│   └── apps/
│       └── spicedb/                      # SpiceDB + PG cluster + backup + network policy
├── scripts/                               # Bootstrap and operational scripts
│   ├── tofu.sh                          # Wrapper: fetches S3 creds from 1Password, runs tofu
│   ├── bootstrap-talos.sh               # Phase 4: generate configs, apply, bootstrap
│   ├── bootstrap-k8s.sh                 # Phase 5: Cilium, democratic-csi, Flux, secrets
│   ├── setup-teleport-kube.sh           # Phase 7: Teleport K8s + Talos integration
│   ├── democratic-csi-values.yaml       # Helm values for ZFS-iSCSI CSI driver
│   └── emergency-recovery.sh            # Emergency SSH via Scaleway console
└── README.md
```

## Prerequisites

Complete all of these before starting any provisioning phase.

### Workstation Tools

Install the following on your local machine:

```bash
# OpenTofu
brew install opentofu          # or see https://opentofu.org/docs/intro/install/

# Talos
brew install siderolabs/tap/talosctl

# Kubernetes
brew install helm kubectl
```

### External Services

1. **Scaleway Elastic Metal** -- server provisioned and accessible via out-of-band console
2. **Scaleway Object Storage** -- create two buckets:
   - `waddle-tofu-state` -- for OpenTofu state
   - `waddle-pg-backups` -- for CloudNativePG backups
3. **Scaleway S3 credentials** -- generate an API key with access to both buckets
4. **Cloudflare** -- `waddle.social` domain added (DNS-only, no orange cloud proxy)
5. **Cloudflare API token** -- with `Zone:DNS:Edit` permission scoped to `waddle.social`
6. **GitHub org** -- `waddle-social` with 2FA enforced for all members
7. **GitHub OAuth App** -- registered under `waddle-social` org settings:
   - Homepage URL: `https://teleport.waddle.social`
   - Callback URL: `https://teleport.waddle.social/v1/webapi/github/callback`
   - Note the **Client ID** and **Client Secret**
8. **GitHub repo** -- `waddle-social/platform` (private), for Flux GitOps
9. **SSH deploy key** -- generate a key pair, add the public key to the `platform` repo as a read-only deploy key:
   ```bash
   ssh-keygen -t ed25519 -f deploy-key -N "" -C "flux-deploy-key"
   ```
10. **1Password** -- Business or Teams account with Secrets Automation enabled:
    - Create a vault named `waddle-platform`
    - Create a Connect server, save the `1password-credentials.json` and access token
    - Populate the vault with items for: Cloudflare API token, SpiceDB preshared key, Scaleway S3 credentials, CloudNativePG superuser password

### Store All Secrets in 1Password

The following secrets are needed across phases. Store them in the `waddle-platform` vault:

| Secret | Used By |
|---|---|
| Proxmox API token | OpenTofu provider |
| Scaleway S3 credentials | OpenTofu state backend, PG backups |
| Cloudflare API token | cert-manager, External DNS |
| GitHub OAuth Client ID + Secret | Teleport SSO |
| 1Password Connect credentials JSON | Cluster bootstrap |
| 1Password Connect access token | Cluster bootstrap |
| SpiceDB preshared key | SpiceDB gRPC auth |
| CloudNativePG superuser password | PostgreSQL |
| SSH deploy key (private) | Flux git sync |

---

## Provisioning Guide

### Phase 0 -- OpenTofu State Backend

Set up the S3 backend so state is stored remotely from the start.

**1. Install the 1Password CLI (`op`):**

```bash
brew install 1password-cli
op signin --account waddle-social.1password.eu
```

**2. Create your `terraform.tfvars`:**

```bash
cp tofu/terraform.tfvars.example tofu/terraform.tfvars
# Edit tofu/terraform.tfvars with your actual values
```

Required variables (see `terraform.tfvars.example` for format):

| Variable | Description |
|---|---|
| `proxmox_endpoint` | Proxmox API URL, e.g. `https://10.10.0.1:8006` |
| `proxmox_api_token` | API token in `user@realm!token=secret` format |
| `public_ip` | Scaleway server's public IP |
| `public_gateway` | Public network gateway |
| `ssh_public_key` | Your SSH public key |
| `operator_ip` | Your current public IP (for temporary SSH) |
| `teleport_github_client_id` | GitHub OAuth App client ID |
| `teleport_github_client_secret` | GitHub OAuth App client secret |
| `talos_schematic_id` | Talos Image Factory schematic ID (see Phase 4) |

**3. Initialize OpenTofu:**

The wrapper script `scripts/tofu.sh` fetches Scaleway S3 credentials from 1Password automatically. Use it for all tofu commands:

```bash
./scripts/tofu.sh init
```

Verify it connects to the S3 backend without errors.

---

### Phase 1 -- Proxmox Host Setup

This phase has both manual steps (run on the Proxmox host) and OpenTofu-managed resources.

#### Manual Steps (via Scaleway out-of-band console or SSH)

**1. Secure the host:**

```bash
# Change root password
passwd

# Add your SSH key
mkdir -p ~/.ssh
echo "ssh-ed25519 AAAA..." >> ~/.ssh/authorized_keys
chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys

# Disable password authentication
sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config
systemctl restart sshd
```

**2. Create the OpenTofu API user and token:**

```bash
pveum user add tofu@pve
pveum aclmod / -user tofu@pve -role PVEAdmin
pveum user token add tofu@pve tofu-token --privsep 0
```

Save the output token in the format `tofu@pve!tofu-token=<uuid>` -- this goes into your `terraform.tfvars` as `proxmox_api_token`.

**3. Create the ZFS dataset for CSI volumes:**

```bash
zfs create rpool/k8s-csi
```

**4. Install and configure iSCSI target (targetcli):**

```bash
apt update && apt install -y targetcli-fb
systemctl enable rtslib-fb-targetctl
systemctl start rtslib-fb-targetctl
```

**5. Enable IP forwarding and NAT:**

```bash
echo 'net.ipv4.ip_forward=1' >> /etc/sysctl.conf
sysctl -p
```

The NAT masquerade rule will be needed once vmbr1 exists. After OpenTofu creates vmbr1, run:

```bash
iptables -t nat -A POSTROUTING -s 10.10.0.0/24 -o vmbr0 -j MASQUERADE

# Persist across reboots
apt install -y iptables-persistent
netfilter-persistent save
```

#### OpenTofu -- Network Module

This creates the internal bridge and enables the Proxmox firewall:

```bash
./scripts/tofu.sh plan -target=module.network
./scripts/tofu.sh apply -target=module.network
```

What it creates:
- `vmbr1` Linux bridge with address `10.10.0.1/24`
- Cluster-level firewall: enabled, default input policy `DROP`
- Node-level firewall rules:
  - Allow all inbound from `vmbr1` interface
  - Allow SSH from your `operator_ip` (temporary, removed in Phase 3)

**Verify:**

```bash
# On Proxmox host
ip addr show vmbr1     # should show 10.10.0.1/24
pvesh get /cluster/firewall/options   # should show enabled
```

---

### Phase 2 -- HAProxy VM

Deploys the public-facing L4 TCP proxy that routes traffic based on SNI headers.

```bash
./scripts/tofu.sh plan -target=module.haproxy
./scripts/tofu.sh apply -target=module.haproxy
```

What it creates:
- Downloads Debian 12 cloud image
- Creates a VM (ID 100): 1 vCPU, 512 MB RAM, 8 GB disk
- Dual-homed: `vmbr0` (public IP) + `vmbr1` (10.10.0.3)
- Cloud-init installs HAProxy and deploys the SNI routing config
- Hardens SSH: password auth disabled, root login disabled
- Creates user `deploy` with your SSH key

**HAProxy routing rules (port 443, TCP mode):**

| SNI Pattern | Backend |
|---|---|
| `teleport.waddle.social` | Teleport VM (10.10.0.2:3080) |
| `proxmox.waddle.social` | Teleport VM (10.10.0.2:3080) |
| `*.apps.waddle.social` | Cilium Gateway VIP (10.10.0.30:443) |
| Default | Drop (no servers) |

Port 80 is forwarded to Cilium Gateway VIP (10.10.0.30:80) for HTTP-to-HTTPS redirect.

**Manual step -- Cloudflare DNS:**

Create two A records pointing to the HAProxy VM's public IP:
- `teleport.waddle.social` -> `<public_ip>`
- `proxmox.waddle.social` -> `<public_ip>`

**Verify:**

```bash
ssh deploy@<public_ip>
sudo haproxy -c -f /etc/haproxy/haproxy.cfg   # config valid
sudo ss -tlnp | grep 443                       # listening
```

---

### Phase 3 -- Teleport VM

Deploys the Teleport access gateway on the internal network only.

```bash
./scripts/tofu.sh plan -target=module.teleport
./scripts/tofu.sh apply -target=module.teleport
```

What it creates:
- Creates a VM (ID 101): 2 vCPU, 2 GB RAM, 20 GB disk
- Internal only: `vmbr1` (10.10.0.2) -- no public interface
- Cloud-init installs Teleport 18.x Community Edition
- Configures:
  - Auth service with GitHub SSO (`waddle-social` org)
  - Proxy service with ACME/Let's Encrypt (works through HAProxy SNI passthrough)
  - SSH service for node access
  - Kubernetes service for cluster access (connects to Talos VIP later)
  - App service: Proxmox UI at `proxmox.waddle.social` -> `https://10.10.0.1:8006`
- Creates the GitHub SSO connector automatically on first boot

**Verify:**

1. Open `https://teleport.waddle.social` in your browser -- the Teleport login page should appear
2. Log in with GitHub SSO -- your `waddle-social` org membership should grant access
3. Navigate to `https://proxmox.waddle.social` -- the Proxmox UI should load through Teleport

**Lockdown -- Remove temporary SSH access:**

After confirming Teleport works, remove the temporary SSH firewall rule. Edit the `operator_ip` variable to a non-routable address or remove the rule via Proxmox UI:

```
Proxmox UI -> Datacenter -> Firewall -> Rules -> delete the "Temporary SSH" rule
```

From this point, SSH to the Proxmox host is only available through Teleport.

**Verify emergency access:**

Confirm the Scaleway out-of-band console still works as a fallback. If you ever lose Teleport access, use `scripts/emergency-recovery.sh` via that console.

---

### Phase 4 -- Talos Kubernetes Cluster

#### Generate the Talos Image Factory Schematic

Before running OpenTofu, you need a schematic ID from [Talos Image Factory](https://factory.talos.dev/) with these extensions:
- `siderolabs/qemu-guest-agent`
- `siderolabs/iscsi-tools`

Visit https://factory.talos.dev/, select version `v1.12.4`, platform `nocloud`, and add both extensions. Copy the schematic ID and set it in your `terraform.tfvars`:

```hcl
talos_schematic_id = "<your-schematic-id>"
```

#### Deploy Talos VMs

```bash
./scripts/tofu.sh plan -target=module.talos_cluster
./scripts/tofu.sh apply -target=module.talos_cluster
```

What it creates:
- Downloads the Talos nocloud image from Image Factory
- Creates 3 VMs (IDs 110-112): 3 vCPU, 8 GB RAM, 10 GB disk each
- All on `vmbr1` with static IPs: 10.10.0.10, 10.10.0.11, 10.10.0.12
- DNS set to 1.1.1.1 and 8.8.8.8

#### Bootstrap the Cluster

After the VMs are running, generate and apply Talos machine configs:

```bash
./scripts/bootstrap-talos.sh
```

This script:
1. Runs `talosctl gen config` to generate base configs and secrets
2. Creates per-node patches with static IPs, VIP (10.10.0.20), disabled CNI, disabled kube-proxy, and allowed scheduling on control planes
3. Applies the patched configs to each node via `talosctl apply-config --insecure`
4. Bootstraps the first node with `talosctl bootstrap`
5. Retrieves the kubeconfig

**Verify:**

```bash
export TALOSCONFIG=talos/generated/talosconfig
talosctl get members --nodes 10.10.0.10
# Should show 3 members

kubectl get nodes
# All 3 nodes in NotReady state (expected -- Cilium not yet installed)
```

> **Important:** The generated `talos/generated/` directory contains cluster secrets (PKI, tokens). It is gitignored. Back up `talos/generated/talosconfig` and `talos/generated/secrets.yaml` securely (e.g., in 1Password).

---

### Phase 5 -- Cluster Bootstrap (Pre-GitOps)

These components must be installed before Flux can operate because the cluster requires a CNI, storage, and the GitOps engine itself.

#### Prepare democratic-csi Values

Edit `scripts/democratic-csi-values.yaml` with the SSH private key for the Proxmox host. The CSI driver needs SSH access to create/destroy ZFS zvols and manage iSCSI targets:

```yaml
driver:
  config:
    sshConnection:
      host: 10.10.0.1
      port: 22
      username: root
      privateKey: |
        -----BEGIN OPENSSH PRIVATE KEY-----
        ... your key ...
        -----END OPENSSH PRIVATE KEY-----
```

> **Security note:** This key grants root SSH to the Proxmox host. Generate a dedicated key pair for this purpose, and restrict it to the commands democratic-csi needs if possible.

#### Run the Bootstrap Script

```bash
cd scripts
./bootstrap-k8s.sh
```

The script installs components in order and prompts for secrets interactively:

**Step 5a -- Cilium 1.19.0:**
- Installs Cilium CNI with kube-proxy replacement, L2 announcements, Gateway API, and external IPs
- Creates `CiliumL2AnnouncementPolicy` and `CiliumLoadBalancerIPPool` for the Gateway VIP (10.10.0.30)
- Waits for all nodes to become `Ready`

**Step 5b -- democratic-csi 0.15.1:**
- Installs the ZFS-generic-iSCSI CSI driver
- Creates the `zfs-iscsi` StorageClass (set as default)
- Reads values from `democratic-csi-values.yaml`

**Step 5c -- Flux Operator 0.40.0:**
- Installs the Flux Operator into `flux-system` namespace

**Step 5d -- Bootstrap Secrets (interactive prompts):**
- Prompts for path to `1password-credentials.json`
- Prompts for 1Password Connect access token (hidden input)
- Prompts for paths to the Flux SSH deploy key (private + public)
- Creates secrets in the `1password` and `flux-system` namespaces

**Step 5e -- FluxInstance:**
- Applies the `FluxInstance` CRD pointing to `ssh://git@github.com/waddle-social/platform.git`
- Flux begins reconciling from the `clusters/scaleway` path on the `main` branch

**Verify:**

```bash
kubectl get nodes                          # all 3 Ready
kubectl get pods -A                        # Cilium, democratic-csi, Flux pods running
kubectl get fluxinstance -n flux-system    # shows Ready

# Test storage
kubectl apply -f - <<EOF
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: test-pvc
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 1Gi
  storageClassName: zfs-iscsi
EOF
kubectl get pvc test-pvc                   # should be Bound
kubectl delete pvc test-pvc                # cleanup
```

---

### Phase 6 -- GitOps Layer

From this point, all infrastructure and application changes go through Git commits to the `waddle-social/platform` repo. Flux reconciles the desired state automatically.

All Flux kustomization manifests are pre-built in the `platform/` directory of this repo. Copy them into the `waddle-social/platform` repo:

```bash
# From the waddle-infra repo root
cp -r platform/* /path/to/waddle-social-platform/
cd /path/to/waddle-social-platform/
git add -A && git commit -m "Add Flux kustomizations for infrastructure and apps"
git push origin main
```

The `platform/` directory contains:

| Path | Contents |
|---|---|
| `clusters/scaleway/infrastructure.yaml` | Flux Kustomization entry point for infra |
| `clusters/scaleway/apps.yaml` | Flux Kustomization entry point for apps (depends on infra) |
| `infrastructure/external-secrets-operator/` | ESO 2.0.0 HelmRelease + HelmRepository |
| `infrastructure/onepassword-connect/` | 1Password Connect 2.3.0 + ClusterSecretStore |
| `infrastructure/cert-manager/` | cert-manager 1.19.3 + ClusterIssuer (DNS-01) + ExternalSecret for Cloudflare token |
| `infrastructure/external-dns/` | External DNS 1.20.0 + ExternalSecret for Cloudflare token |
| `infrastructure/cloudnative-pg/` | CloudNativePG 1.28.1 operator |
| `infrastructure/spicedb-operator/` | SpiceDB Operator 1.22.0 |
| `infrastructure/cilium-gateway/` | Gateway + wildcard Certificate for `*.apps.waddle.social` |
| `apps/spicedb/` | SpiceDB instance + PG Cluster (2 instances, WAL+base backup to S3) + GRPCRoute + CiliumNetworkPolicy (Cloudflare IPs only) |

#### Flux Kustomization Dependency Chain

Components must be deployed in order. Use `dependsOn` in Flux Kustomizations:

```
external-secrets-operator
  └── onepassword-connect
      ├── cert-manager           (needs Cloudflare token from ESO)
      ├── external-dns           (needs Cloudflare token from ESO)
      ├── cloudnative-pg         (operator only, no secrets needed yet)
      └── spicedb-operator       (operator only)
          └── apps/spicedb       (needs PG running + preshared key from ESO)
```

Cilium Gateway HTTPRoutes depend on cert-manager being ready (for the `*.apps.waddle.social` wildcard certificate).

#### What Each Component Does

| Component | Purpose |
|---|---|
| **External Secrets Operator** | Syncs secrets from 1Password into Kubernetes Secrets |
| **1Password Connect** | In-cluster bridge to 1Password Cloud (connect-api + connect-sync pods) |
| **cert-manager** | Issues `*.apps.waddle.social` wildcard cert via Let's Encrypt DNS-01 |
| **External DNS** | Creates Cloudflare DNS records for services with matching annotations |
| **CloudNativePG** | PostgreSQL operator -- runs 1 primary + 1 replica for SpiceDB |
| **SpiceDB Operator** | Manages SpiceDB instances backed by CloudNativePG Postgres |
| **Cilium Gateway** | Gateway API HTTPRoutes for `*.apps.waddle.social` traffic |

#### Verify Phase 6

```bash
kubectl get helmrelease -A                 # all show Ready
kubectl get externalsecret -A              # all show SecretSynced
kubectl get certificate -A                 # wildcard cert issued
kubectl get clusters.postgresql.cnpg.io -A # PG cluster healthy
kubectl get spicedbs -A                    # SpiceDB running

# Test SpiceDB connectivity
grpcurl -insecure \
  -H "authorization: Bearer <preshared-key>" \
  spicedb.apps.waddle.social:443 \
  grpc.health.v1.Health/Check
```

---

### Phase 7 -- Teleport Kubernetes and Talos Integration

After the cluster is running, configure Teleport to provide kubectl and talosctl access through its zero-trust tunnel.

**Run the setup script:**

```bash
# Make sure you're logged into Teleport first
tsh login --proxy=teleport.waddle.social

# Run the Phase 7 script
./scripts/setup-teleport-kube.sh
```

This script:
1. Generates a kubeconfig from the Talos cluster (pointing at VIP 10.10.0.20)
2. Copies the kubeconfig to the Teleport VM via `tsh scp`
3. Updates `/etc/teleport.yaml` on the Teleport VM to reference the kubeconfig
4. Restarts Teleport and verifies the cluster appears in `tsh kube ls`
5. Tests `kubectl get nodes` through the Teleport tunnel

**Verify kubectl through Teleport:**

```bash
tsh login --proxy=teleport.waddle.social
tsh kube ls                                # should show waddle-cluster
tsh kube login waddle-cluster
kubectl get nodes                          # works through Teleport
```

**Verify talosctl through Teleport:**

Use Teleport's kube proxy to tunnel talosctl traffic to the internal network:

```bash
# Start a Teleport kube proxy in the background
tsh proxy kube --port=6443 &

# Use talosctl through the tunnel
talosctl --endpoints 127.0.0.1 --nodes 10.10.0.10 get members
talosctl --endpoints 127.0.0.1 --nodes 10.10.0.10 version
```

**Access the Proxmox host via Teleport SSH:**

```bash
tsh ssh root@proxmox-host
```

---

## Emergency Recovery

If the Teleport VM is down and you need access to the Proxmox host:

1. Log into the **Scaleway out-of-band console** (always available regardless of VM state)
2. Run the emergency recovery script:
   ```bash
   /root/emergency-recovery.sh
   ```
   Or manually add a temporary firewall rule:
   ```bash
   pvesh create /nodes/$(hostname)/firewall/rules \
     --action ACCEPT --type in \
     --source <your-ip> --dport 22 --proto tcp \
     --comment "EMERGENCY" --enable 1 --pos 0
   ```
3. SSH in and fix the issue
4. **Remove the emergency rule immediately after recovery**

> **Note:** While the Teleport VM is down, HAProxy (which runs on a separate VM) continues to serve public traffic to `*.apps.waddle.social`. Only management access (Proxmox UI, kubectl, talosctl) is affected.

## Disaster Recovery -- Full Rebuild

If the entire Proxmox host is lost:

1. Provision a new Scaleway Elastic Metal server
2. Complete Phase 1 manual steps (secure host, create API user, ZFS, iSCSI)
3. Run `./scripts/tofu.sh apply` -- recreates all VMs (network, HAProxy, Teleport, Talos nodes)
4. Run `scripts/bootstrap-talos.sh` -- re-bootstrap the Talos cluster
5. Run `scripts/bootstrap-k8s.sh` -- re-install Cilium, democratic-csi, Flux, bootstrap secrets
6. Flux syncs from Git -- all infrastructure and apps are restored
7. Restore PostgreSQL data from the Scaleway Object Storage backup:
   ```yaml
   # In the CloudNativePG Cluster CR, add a recovery section:
   spec:
     bootstrap:
       recovery:
         source: clusterBackup
     externalClusters:
       - name: clusterBackup
         barmanObjectStore:
           destinationPath: s3://waddle-pg-backups/
           endpointURL: https://s3.fr-par.scw.cloud
           s3Credentials:
             accessKeyId:
               name: scaleway-s3-creds
               key: ACCESS_KEY_ID
             secretAccessKey:
               name: scaleway-s3-creds
               key: SECRET_ACCESS_KEY
   ```
8. Re-apply the two bootstrap secrets (1Password Connect credentials)

## Security Notes

- **No secrets in Git** -- all secrets live in 1Password, synced via External Secrets Operator
- **Teleport VM has no public interface** -- only reachable via the internal network through HAProxy
- **Proxmox host is firewalled** -- default DROP on public interface, only vmbr1 traffic allowed
- **SpiceDB is hardened** -- preshared key required for all API calls, CiliumNetworkPolicy restricts ingress to Cloudflare IP ranges only
- **Talos uses mTLS** -- API and etcd communication is mutually authenticated
- **Cloudflare API token is scoped** -- limited to DNS edits on `apps.waddle.social` only
- **SSH deploy key is read-only** -- Flux cannot write to the platform repo
- **GitHub SSO enforces 2FA** -- at the org level for all `waddle-social` members
- **OpenTofu state is remote** -- stored in Scaleway Object Storage, not on any VM
