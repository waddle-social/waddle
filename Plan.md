# waddle.social Platform Architecture Plan

## Overview

Single Proxmox host on Scaleway Elastic Metal running a 3-node Talos Linux Kubernetes cluster, with Teleport as the sole internet-facing entry point. All cluster services are managed via GitOps using Flux Operator.

---

## Infrastructure

| Component          | Detail                                                     |
| ------------------ | ---------------------------------------------------------- |
| Host               | Scaleway Elastic Metal — AMD Ryzen PRO 3600, 32 GB RAM    |
| Storage            | 2× 1 TB NVMe in ZFS mirror (~1 TB usable)                 |
| Hypervisor         | Proxmox VE (ZFS root)                                     |
| Provisioning       | OpenTofu with bpg/proxmox provider                        |
| ToFu state         | Stored on Teleport VM (single operator)                    |
| Domain             | `waddle.social` (Cloudflare, DNS-only — no orange cloud)   |
| Secrets            | 1Password → External Secrets Operator                      |
| Backups            | CloudNativePG WAL + base backups → Scaleway Object Storage |
| Monitoring         | Deferred (add later)                                       |

---

## Network Architecture

### Bridges

| Bridge | Range            | Purpose                              |
| ------ | ---------------- | ------------------------------------ |
| vmbr0  | Public IP        | Internet-facing (Teleport VM only)   |
| vmbr1  | 10.10.0.0/24     | Internal (all VMs, iSCSI, K8s)      |

### IP Assignments

| Host              | IP           | Network       |
| ----------------- | ------------ | ------------- |
| Proxmox host      | 10.10.0.1    | vmbr1         |
| Teleport VM       | public IP    | vmbr0         |
| Teleport VM       | 10.10.0.2    | vmbr1         |
| Talos CP1         | 10.10.0.10   | vmbr1         |
| Talos CP2         | 10.10.0.11   | vmbr1         |
| Talos CP3         | 10.10.0.12   | vmbr1         |
| Talos VIP (K8s)   | 10.10.0.20   | vmbr1 (float) |
| Cilium Gateway VIP| 10.10.0.30   | vmbr1 (L2)    |

### Outbound NAT

Proxmox host runs masquerade NAT on vmbr1 so Talos nodes can reach the internet (pull images, Flux sync, DNS-01 challenges). Talos nodes have no dependency on the Teleport VM for outbound traffic.

### DNS Records

| FQDN                     | Type | Value     | Managed by   |
| ------------------------ | ---- | --------- | ------------ |
| `teleport.waddle.social` | A    | Public IP | Manual       |
| `proxmox.waddle.social`  | A    | Public IP | Manual       |
| `*.apps.waddle.social`   | A    | Public IP | External DNS |

External DNS Cloudflare API token is scoped to manage only the `apps.waddle.social` subdomain zone.

---

## Traffic Flow

All external traffic enters through the Teleport VM's public IP. HAProxy runs in TCP/SNI mode on port 443, routing based on the TLS ClientHello SNI header. No TLS termination happens at HAProxy — it's a pure L4 router.

```
Internet (public IP on Teleport VM)
│
├── :443 → HAProxy (TCP/SNI mode)
│          ├── SNI: teleport.waddle.social  → Teleport Proxy (:3080)
│          ├── SNI: proxmox.waddle.social   → Teleport Proxy (:3080) → Proxmox :8006
│          ├── SNI: *.apps.waddle.social    → Cilium Gateway VIP (10.10.0.30:443)
│          └── default                      → drop
│
├── :80  → Cilium Gateway VIP (10.10.0.30:80) for HTTP→HTTPS redirect
│
└── Teleport internal
           ├── kubectl access → Talos VIP (10.10.0.20:6443)
           └── Talos API      → individual node IPs
```

For public Kubernetes services (`*.apps.waddle.social`), HAProxy forwards traffic directly to the Cilium Gateway VIP — Teleport is not in the path. TLS is terminated by cert-manager certificates at the Cilium Gateway API level.

For Teleport-protected services (Proxmox UI, kubectl, Talos API), all access requires GitHub SSO authentication through Teleport.

---

## VM Layout

| VM         | vCPU | RAM  | OS Disk | Network        |
| ---------- | ---- | ---- | ------- | -------------- |
| Teleport   | 2    | 2 GB | 20 GB   | vmbr0 + vmbr1  |
| Talos CP1  | 3    | 8 GB | 10 GB   | vmbr1          |
| Talos CP2  | 3    | 8 GB | 10 GB   | vmbr1          |
| Talos CP3  | 3    | 8 GB | 10 GB   | vmbr1          |
| **Totals** | **11** | **26 GB** | **50 GB** |         |

Remaining for Proxmox host: 6 GB RAM, ~950 GB ZFS for CSI volumes.

All 3 Talos nodes are combined control plane + worker nodes (default taint removed to allow scheduling workloads).

---

## Talos Configuration

| Setting                 | Value                                       |
| ----------------------- | ------------------------------------------- |
| Version                 | v1.12.4                                     |
| Kubernetes              | 1.35.0 (bundled with Talos v1.12.x)        |
| Image source            | Talos Image Factory                         |
| Image Factory extensions| `siderolabs/qemu-guest-agent`, `siderolabs/iscsi-tools` |
| Control plane endpoint  | `10.10.0.20:6443` (Talos shared VIP)        |
| CNI                     | None (Talos default disabled, Cilium installed separately) |
| Scheduling              | Workloads allowed on control plane nodes    |

The `qemu-guest-agent` extension is required for OpenTofu/Proxmox to read VM IPs and perform graceful operations. The `iscsi-tools` extension is required for democratic-csi iSCSI volumes.

---

## Storage Architecture

### ZFS Layout

```
rpool                  (mirror, ~1 TB usable)
├── rpool/ROOT         (Proxmox OS)
├── rpool/data         (VM disks)
└── rpool/k8s-csi      (dedicated dataset for democratic-csi zvols)
```

### CSI Driver

| Setting        | Value                                          |
| -------------- | ---------------------------------------------- |
| Driver         | democratic-csi                                 |
| Mode           | zfs-generic-iscsi                              |
| Storage        | zvols created under `rpool/k8s-csi`            |
| Protocol       | iSCSI (targetcli on Proxmox host)              |
| Portal         | `10.10.0.1:3260`                               |

iSCSI provides block storage, which is important for CloudNativePG/PostgreSQL performance. The Proxmox host runs `targetcli` to export zvols as iSCSI targets. Talos nodes connect as iSCSI initiators via the `iscsi-tools` extension.

---

## Teleport Configuration

### Edition and Auth

| Setting        | Value                                     |
| -------------- | ----------------------------------------- |
| Edition        | Community (self-hosted)                   |
| Auth method    | GitHub SSO                                |
| GitHub org     | `waddle-social`                           |
| Access rule    | All org owners → `admin` role             |
| OAuth App      | Registered under `waddle-social` org      |

Requires a GitHub OAuth App in `waddle-social` org settings with callback URL: `https://teleport.waddle.social/v1/webapi/github/callback`

### Access Matrix

| Resource                | Access method                    |
| ----------------------- | -------------------------------- |
| Proxmox UI (:8006)      | Teleport App Access              |
| kubectl (K8s API)       | Teleport Kubernetes Access       |
| Talos API (talosctl)    | Teleport tunneled access         |
| SSH to Teleport VM      | Teleport Node Access             |
| Public K8s services     | Direct (bypasses Teleport)       |

### HAProxy on Teleport VM

HAProxy runs in TCP mode with SNI-based routing. It does **not** terminate TLS.

```
frontend ft_ssl
    bind *:443
    mode tcp
    tcp-request inspect-delay 5s
    tcp-request content accept if { req_ssl_hello_type 1 }

    use_backend bk_teleport if { req_ssl_sni -i teleport.waddle.social }
    use_backend bk_teleport if { req_ssl_sni -i proxmox.waddle.social }
    use_backend bk_k8s_ingress if { req_ssl_sni -m end .apps.waddle.social }
    default_backend bk_drop

backend bk_teleport
    mode tcp
    server teleport 127.0.0.1:3080

backend bk_k8s_ingress
    mode tcp
    server cilium_gw 10.10.0.30:443

backend bk_drop
    mode tcp
    # no servers — connection drops

frontend ft_http
    bind *:80
    mode tcp
    default_backend bk_k8s_ingress_http

backend bk_k8s_ingress_http
    mode tcp
    server cilium_gw 10.10.0.30:80
```

---

## TLS and Certificate Management

| Scope                     | Method                                |
| ------------------------- | ------------------------------------- |
| `teleport.waddle.social`  | Teleport built-in ACME (Let's Encrypt)|
| `*.apps.waddle.social`    | cert-manager, DNS-01 via Cloudflare   |
| Proxmox UI                | Self-signed (behind Teleport, not exposed) |
| Talos API / etcd          | Talos auto-generated mTLS             |

### cert-manager Configuration

- ClusterIssuer using Let's Encrypt production with DNS-01 solver
- Cloudflare API token with `Zone:DNS:Edit` permission for `waddle.social`
- Wildcard certificate for `*.apps.waddle.social`
- Same Cloudflare API token used by External DNS (stored in 1Password → ESO)

---

## Secrets Management

### Architecture

```
1Password Cloud
    ↕ (sync)
1Password Connect Server (in-cluster pods: connect-api + connect-sync)
    ↕ (REST API)
External Secrets Operator
    ↓ (creates native Kubernetes Secrets)
Consumers: cert-manager, external-dns, democratic-csi, SpiceDB, CNPG, etc.
```

### Bootstrap Secrets (manually applied once)

Only two secrets need manual application during initial cluster setup:

1. `1password-credentials.json` — Connect server identity
2. Connect access token — ESO authenticates to Connect

Everything else flows through `ExternalSecret` CRDs in the Git repo, referencing 1Password vault items by path. No secret values in Git.

### 1Password Vault Structure

Recommended vault: `waddle-platform` containing items for:

- Cloudflare API token (used by cert-manager + External DNS)
- SpiceDB preshared key
- democratic-csi iSCSI CHAP credentials (if using CHAP)
- CloudNativePG superuser password
- Scaleway Object Storage S3 credentials (for PG backups)
- Teleport join tokens (if needed)

---

## SpiceDB Architecture

| Setting             | Value                                          |
| ------------------- | ---------------------------------------------- |
| Operator            | SpiceDB Operator (authzed)                     |
| Database            | PostgreSQL via CloudNativePG                   |
| PG topology         | 1 primary + 1 replica                          |
| PG storage          | democratic-csi iSCSI PVCs (~10-20 GB)         |
| PG backups          | WAL + base backups → Scaleway Object Storage   |
| Auth                | Preshared key (from 1Password → ESO)           |
| Public exposure     | `spicedb.apps.waddle.social` via Cilium Gateway|
| Network restriction | CiliumNetworkPolicy: allow Cloudflare IP ranges only |

### Security Hardening

SpiceDB gRPC endpoint is publicly exposed for Cloudflare Workers. Hardening:

1. **Preshared key** — bearer token required for all API calls
2. **Cloudflare IP restriction** — CiliumNetworkPolicy limits ingress to Cloudflare's published egress IP ranges (https://www.cloudflare.com/ips/)
3. **TLS** — cert-manager wildcard certificate terminates at Cilium Gateway

---

## Kubernetes Components

### Bootstrap Layer (installed before GitOps)

These are installed via Helm/talosctl during initial cluster setup because the cluster cannot function without them:

1. **Cilium** — CNI, L2 announcements, Gateway API CRDs
2. **democratic-csi** — storage, iSCSI to Proxmox host
3. **Flux Operator** — GitOps engine
4. **FluxInstance CRD** — points Flux to the GitHub repo

### GitOps-Managed Layer (Flux manages after bootstrap)

```
repo: waddle-social/platform
├── clusters/
│   └── scaleway/
│       └── flux-instance.yaml
├── infrastructure/
│   ├── cert-manager/              # Let's Encrypt DNS-01
│   ├── external-dns/              # Cloudflare DNS management
│   ├── external-secrets-operator/ # ESO controller
│   ├── onepassword-connect/       # 1Password Connect server
│   ├── cloudnative-pg/            # PostgreSQL operator
│   ├── spicedb-operator/          # SpiceDB operator
│   └── cilium-gateway/            # Gateway API routes, HTTPRoute configs
└── apps/
    └── spicedb/
        ├── spicedb-instance.yaml  # SpiceDB CR
        └── pg-cluster.yaml        # CloudNativePG Cluster CR
```

### Flux Kustomization Dependencies

```
infrastructure/external-secrets-operator
    └── infrastructure/onepassword-connect
        └── infrastructure/cert-manager        (needs Cloudflare token from ESO)
        └── infrastructure/external-dns        (needs Cloudflare token from ESO)
        └── infrastructure/cloudnative-pg      (operator only, no secrets needed)
        └── infrastructure/spicedb-operator    (operator only)
            └── apps/spicedb                   (needs PG running, preshared key from ESO)
```

Cilium Gateway routes depend on cert-manager being ready (for TLS certificates).

---

## Provisioning Sequence

### Phase 0 — Immediate Host Lockdown

**Goal:** Secure the Proxmox host the moment it's accessible.

1. SSH into Proxmox via Scaleway out-of-band console
2. Apply immediate iptables rules:
   ```bash
   # Block Proxmox UI from public internet
   iptables -A INPUT -p tcp --dport 8006 -j DROP
   # Allow SSH (temporary — will be locked down after Teleport)
   iptables -A INPUT -p tcp --dport 22 -j ACCEPT
   # Allow established connections
   iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
   # Allow loopback
   iptables -A INPUT -i lo -j ACCEPT
   # Drop everything else
   iptables -A INPUT -j DROP
   ```
3. Change default root password
4. Disable SSH password authentication
5. Add your SSH public key

### Phase 1 — Network Setup

1. Create internal bridge `vmbr1` (10.10.0.0/24, no physical interface)
2. Assign `10.10.0.1/24` to vmbr1 on the Proxmox host
3. Enable IP forwarding and masquerade NAT:
   ```bash
   echo 'net.ipv4.ip_forward=1' >> /etc/sysctl.conf
   sysctl -p
   iptables -t nat -A POSTROUTING -s 10.10.0.0/24 -o vmbr0 -j MASQUERADE
   ```
4. Create ZFS dataset: `zfs create rpool/k8s-csi`
5. Install and configure `targetcli` for iSCSI target service
6. Persist iptables rules

### Phase 2 — Teleport VM

1. **OpenTofu:** Create Ubuntu/Debian VM on vmbr0 + vmbr1
2. Install Teleport Community Edition
3. Configure Teleport:
   - Domain: `teleport.waddle.social`
   - ACME/Let's Encrypt for TLS
   - GitHub SSO connector (waddle-social org owners → admin)
4. Install and configure HAProxy with SNI routing (see config above)
5. Configure Teleport App Access for Proxmox UI (`proxmox.waddle.social` → `https://10.10.0.1:8006`)
6. Create DNS records manually in Cloudflare:
   - `teleport.waddle.social` → public IP
   - `proxmox.waddle.social` → public IP
7. Store OpenTofu state on Teleport VM

### Phase 3 — Full Lockdown

1. Remove all direct public access to Proxmox host:
   ```bash
   # Only allow traffic from vmbr1 and established connections
   iptables -F INPUT
   iptables -A INPUT -i lo -j ACCEPT
   iptables -A INPUT -i vmbr1 -j ACCEPT
   iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
   iptables -A INPUT -j DROP
   ```
2. SSH to Proxmox host now only via Teleport
3. Verify Proxmox UI only accessible through Teleport App Access
4. Verify Scaleway out-of-band console still works (emergency access)

### Phase 4 — Talos Cluster

1. Generate Image Factory schematic:
   - Extensions: `qemu-guest-agent`, `iscsi-tools`
   - Version: v1.12.4
   - Platform: `nocloud` (for Proxmox)
2. Download the nocloud image and upload to Proxmox
3. **OpenTofu:** Create 3 Talos VMs on vmbr1 with static IPs
4. Generate Talos machine configs:
   ```bash
   talosctl gen config waddle-cluster https://10.10.0.20:6443 \
     --with-docs=false \
     --with-examples=false
   ```
5. Customize machine configs:
   - Set static IPs (10.10.0.10-12)
   - Configure Talos VIP (10.10.0.20)
   - Disable default CNI (Cilium will be installed separately)
   - Remove control plane taints (allow workload scheduling)
   - Set default gateway to 10.10.0.1
   - Set DNS to external resolver (e.g., 1.1.1.1)
6. Apply configs: `talosctl apply-config --insecure --nodes <ip> --file <config>`
7. Bootstrap first node: `talosctl bootstrap --nodes 10.10.0.10`
8. Retrieve kubeconfig: `talosctl kubeconfig --nodes 10.10.0.10`
9. Verify nodes ready (they'll be NotReady until Cilium is installed)

### Phase 5 — Cluster Bootstrap (pre-GitOps)

These components are installed manually because the cluster cannot function without them.

**5a. Cilium**
```bash
helm install cilium cilium/cilium \
  --namespace kube-system \
  --set ipam.mode=kubernetes \
  --set l2announcements.enabled=true \
  --set externalIPs.enabled=true \
  --set gatewayAPI.enabled=true \
  --set kubeProxyReplacement=true \
  --set k8sServiceHost=10.10.0.20 \
  --set k8sServicePort=6443
```
Configure CiliumL2AnnouncementPolicy and CiliumLoadBalancerIPPool for VIP 10.10.0.30.

**5b. democratic-csi**
```bash
helm install zfs-iscsi democratic-csi/democratic-csi \
  --namespace democratic-csi --create-namespace \
  --values democratic-csi-values.yaml
```
Values configure connection to Proxmox host (10.10.0.1), ZFS dataset (`rpool/k8s-csi`), and iSCSI target settings.

**5c. Flux Operator**
```bash
helm install flux-operator oci://ghcr.io/controlplaneio-fluxcd/charts/flux-operator \
  --namespace flux-system --create-namespace
```

**5d. Bootstrap secrets (manual, one-time)**
```bash
# 1Password Connect credentials
kubectl create secret generic onepassword-credentials \
  --namespace 1password \
  --from-file=1password-credentials.json

# 1Password Connect token
kubectl create secret generic onepassword-token \
  --namespace 1password \
  --from-literal=token=<connect-token>
```

**5e. FluxInstance**
```bash
kubectl apply -f - <<EOF
apiVersion: fluxcd.controlplane.io/v1
kind: FluxInstance
metadata:
  name: flux
  namespace: flux-system
spec:
  distribution:
    version: "2.x"
    registry: "ghcr.io/fluxcd"
  components:
    - source-controller
    - kustomize-controller
    - helm-controller
    - notification-controller
  cluster:
    type: kubernetes
    networkPolicy: true
  sync:
    kind: GitRepository
    url: "ssh://git@github.com/waddle-social/platform.git"
    ref: "refs/heads/main"
    path: "clusters/scaleway"
EOF
```

Create deploy key or use GitHub token for Flux to access the repo.

### Phase 6 — GitOps Takes Over

From this point, all changes are made via Git commits to the `waddle-social/platform` repo. Flux reconciles:

1. External Secrets Operator + 1Password Connect
2. cert-manager (with Cloudflare credentials from ESO)
3. External DNS (with Cloudflare credentials from ESO)
4. CloudNativePG operator
5. SpiceDB operator
6. Cilium Gateway API routes (HTTPRoute for `*.apps.waddle.social`)
7. SpiceDB instance + PG cluster + backup config

---

## Security Posture

| Concern                     | Mitigation                                                    |
| --------------------------- | ------------------------------------------------------------- |
| Proxmox UI exposure         | Behind Teleport App Access, no direct public access           |
| Talos API                   | Internal network only, via Teleport tunnel                    |
| Kubernetes API              | Internal VIP (10.10.0.20), via Teleport Kubernetes Access     |
| SSH to Proxmox              | Disabled publicly after Phase 3, only via Teleport            |
| SpiceDB public gRPC         | Preshared key + Cloudflare IP restriction (CiliumNetworkPolicy)|
| Secrets in Git              | None — all secrets in 1Password, synced via ESO               |
| TLS                         | cert-manager DNS-01 (Let's Encrypt) for public services       |
| Teleport TLS                | Built-in ACME / Let's Encrypt                                 |
| Talos mTLS                  | Auto-generated mutual TLS for API and etcd                    |
| Cloudflare API token scope  | Limited to `apps.waddle.social` DNS edits only                |
| Emergency access            | Scaleway out-of-band console (always available)               |
| GitHub SSO                  | waddle-social org owners only, GitHub 2FA enforced            |
| Network segmentation        | Talos nodes on isolated vmbr1, no public interface            |
| iSCSI traffic               | Isolated to vmbr1 (10.10.0.0/24)                             |

---

## Disaster Recovery

### What's recoverable

- **Cluster state** — fully reproducible from Git repo + OpenTofu configs
- **Secrets** — stored in 1Password, never lost with cluster
- **SpiceDB data** — CloudNativePG continuous backup to Scaleway Object Storage (WAL + base backups)
- **Teleport state** — Teleport stores state locally; if VM is lost, re-provision and re-configure GitHub SSO

### Full rebuild procedure

1. Re-install Proxmox on Scaleway Elastic Metal
2. Run Phase 0-3 (network, Teleport)
3. Run Phase 4-5 (Talos cluster, bootstrap components)
4. Flux syncs from Git — all infrastructure and apps restored
5. Restore PG data from Scaleway Object Storage backup
6. Manually re-apply two bootstrap secrets (1Password Connect credentials)

### Single points of failure

- **Proxmox host** — single physical machine. Scaleway SLA applies.
- **Teleport VM** — if down, no access to Proxmox UI or kubectl (but Scaleway console still works, and public K8s services still route via HAProxy if HAProxy process survives... but it runs on Teleport VM, so it's also down). This means a Teleport VM failure takes down both management access AND public ingress.

**Mitigation for Teleport VM failure:** Keep emergency iptables rules documented that can be applied via Scaleway console to temporarily expose Proxmox UI for recovery.

---

## Component Version Matrix

| Component            | Version / Source                                    |
| -------------------- | --------------------------------------------------- |
| Proxmox VE           | Latest stable                                       |
| Talos Linux          | v1.12.4 (Image Factory)                             |
| Kubernetes           | 1.35.0 (bundled with Talos)                         |
| Cilium               | Latest stable (Helm)                                |
| democratic-csi       | Latest stable (Helm)                                |
| Flux Operator        | Latest stable (OCI Helm chart)                      |
| cert-manager         | Latest stable (Helm, Flux-managed)                  |
| External DNS         | Latest stable (Helm, Flux-managed)                  |
| External Secrets Op. | Latest stable (Helm, Flux-managed)                  |
| 1Password Connect    | Latest stable (Helm, Flux-managed)                  |
| CloudNativePG        | Latest stable (Helm, Flux-managed)                  |
| SpiceDB Operator     | Latest stable (Helm, Flux-managed)                  |
| HAProxy              | OS package on Teleport VM                           |
| Teleport             | Latest Community Edition                            |
| OpenTofu             | Latest stable (workstation tool)                    |

---

## Prerequisites Checklist

Before starting provisioning:

- [ ] Scaleway Elastic Metal server provisioned and accessible
- [ ] `waddle.social` domain in Cloudflare (DNS-only)
- [ ] Cloudflare API token with `Zone:DNS:Edit` for `waddle.social`
- [ ] GitHub org `waddle-social` with team structure and 2FA enforced
- [ ] GitHub OAuth App registered under `waddle-social` for Teleport
- [ ] GitHub repo `waddle-social/platform` created (private)
- [ ] 1Password Business/Teams account with Secrets Automation enabled
- [ ] 1Password Connect server created, credentials JSON + token saved
- [ ] 1Password vault `waddle-platform` with all required secrets
- [ ] Scaleway Object Storage bucket created for PG backups
- [ ] Scaleway S3 credentials generated and stored in 1Password
- [ ] OpenTofu installed on workstation
- [ ] `talosctl` installed on workstation
- [ ] `helm` installed on workstation
- [ ] `kubectl` installed on workstation
