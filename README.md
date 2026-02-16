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
pveum aclmod / -user tofu@pve -role Administrator
pveum user token add tofu@pve tofu-token --privsep 0
```

> **Note:** The `Administrator` role is required (not `PVEAdmin`) because network and firewall operations need `Sys.Modify` privileges.

Save the output token in the format `tofu@pve!tofu-token=<uuid>` -- this goes into your `terraform.tfvars` as `proxmox_api_token`.

**3. Create the ZFS dataset for CSI volumes:**

```bash
# Check your ZFS pool name first: zpool list
# The pool name varies by installation (e.g. zpve, rpool, tank)
zfs create <pool>/k8s-csi
zfs create <pool>/k8s-csi-snaps
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

**6. Configure Proxmox storage content types:**

The `local` directory storage needs additional content types enabled for cloud images and cloud-init snippets:

```bash
pvesm set local --content iso,vztmpl,snippets,import,backup,images
```

**7. Ensure hostname resolution:**

The Proxmox host must be able to resolve its own hostname:

```bash
echo "127.0.1.1 $(hostname)" >> /etc/hosts
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

> **Known issue:** The bpg/proxmox provider's `import_from` and `file_id` disk import methods cause kernel panics on ZFS-backed datastores. The VM must be created manually using `qm importdisk` and then imported into OpenTofu state. See the manual steps below.

**1. Ensure the cloud-init snippet is uploaded (tofu handles this):**

```bash
./scripts/tofu.sh apply -target=module.haproxy.proxmox_virtual_environment_file.haproxy_cloud_config
./scripts/tofu.sh apply -target=module.haproxy.proxmox_virtual_environment_download_file.debian_cloud_image
```

**2. Create the VM manually on the Proxmox host:**

```bash
# Download the cloud image (if not already present)
wget -O /tmp/debian-12-generic-amd64.qcow2 \
  https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2

# Create VM, import disk, configure boot
qm create 100 --memory 2048 --cores 1 --name haproxy \
  --net0 virtio,bridge=vmbr0 --net1 virtio,bridge=vmbr1 \
  --scsihw virtio-scsi-pci --agent enabled=1 \
  --onboot 1 --tags "infra;haproxy"
qm importdisk 100 /tmp/debian-12-generic-amd64.qcow2 vmdata
qm set 100 --scsi0 vmdata:vm-100-disk-0,discard=on,iothread=1
qm resize 100 scsi0 8G
qm set 100 --boot c --bootdisk scsi0
qm set 100 --ide2 local:cloudinit
qm set 100 --ipconfig0 ip=<public_ip>/32,gw=<public_gateway>
qm set 100 --ipconfig1 ip=10.10.0.3/24,gw=10.10.0.1
qm set 100 --cicustom "user=local:snippets/haproxy-cloud-init.yaml"
qm start 100
```

**3. Import the VM into OpenTofu state:**

```bash
./scripts/tofu.sh import module.haproxy.proxmox_virtual_environment_vm.haproxy waddle-proxmox01/100
```

What it creates:
- Debian 12 VM (ID 100): 1 vCPU, 2 GB RAM, 8 GB disk
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
ssh -J root@<public_ip> deploy@10.10.0.3
sudo haproxy -c -f /etc/haproxy/haproxy.cfg   # config valid
sudo ss -tlnp | grep -E '443|80'              # listening on both ports
```

---

### Phase 3 -- Teleport VM

Deploys the Teleport access gateway on the internal network only.

> **Known issue:** Same bpg/proxmox ZFS kernel panic as Phase 2. Create the VM manually with `qm importdisk`, then import into tofu state.

**1. Upload cloud-init snippet (tofu handles this):**

```bash
./scripts/tofu.sh apply -target=module.teleport.proxmox_virtual_environment_file.teleport_cloud_config
```

**2. Create the VM manually on the Proxmox host:**

```bash
qm create 101 --memory 2048 --cores 2 --name teleport \
  --net0 virtio,bridge=vmbr1 \
  --scsihw virtio-scsi-pci --agent enabled=1 \
  --onboot 1 --tags "infra;teleport"
qm importdisk 101 /tmp/debian-12-generic-amd64.qcow2 vmdata
qm set 101 --scsi0 vmdata:vm-101-disk-0,discard=on,iothread=1
qm resize 101 scsi0 20G
qm set 101 --boot c --bootdisk scsi0
qm set 101 --ide2 local:cloudinit
qm set 101 --ipconfig0 ip=10.10.0.2/24,gw=10.10.0.1
qm set 101 --cicustom "user=local:snippets/teleport-cloud-init.yaml"
qm start 101
```

**3. Import into tofu state:**

```bash
./scripts/tofu.sh import module.teleport.proxmox_virtual_environment_vm.teleport waddle-proxmox01/101
```

**4. Install packages manually (cloud-init bug):**

Cloud-init on Debian 12 (v22.4.2) may skip `runcmd`. SSH in and install manually:

```bash
ssh -J root@<public_ip> deploy@10.10.0.2

# Install Teleport
curl -fsSL https://goteleport.com/static/install.sh | sudo bash -s 18
sudo systemctl enable teleport
sudo systemctl start teleport

# Wait for auth service to start, then create the role and connector
sleep 10
sudo tctl create /tmp/infra-admin-role.yaml
sudo tctl create /tmp/github-connector.yaml
```

What it creates:
- Debian 12 VM (ID 101): 2 vCPU, 2 GB RAM, 20 GB disk
- Internal only: `vmbr1` (10.10.0.2) -- no public interface
- Teleport 18.x Community Edition with:
  - Auth service with GitHub SSO (`waddle-social` org, `infra` team)
  - Proxy service with ACME/Let's Encrypt TLS (via HAProxy SNI passthrough)
  - Reverse tunnel listener on port 3024 (required for app/SSH access)
  - SSH service for node access
  - App service: Proxmox UI at `proxmox.waddle.social` -> `https://10.10.0.1:8006`
- Custom `infra-admin` role granting SSH logins (`deploy`, `root`), full app/k8s access
- GitHub SSO connector mapping `waddle-social/infra` team to `infra-admin` role

#### GitHub SSO Setup Requirements

The GitHub OAuth App and org must be configured correctly for SSO to work:

1. **GitHub OAuth App** must be created under the `waddle-social` org (not a personal account):
   - Homepage URL: `https://teleport.waddle.social`
   - Callback URL: `https://teleport.waddle.social/v1/webapi/github/callback`

2. **The `waddle-social` org must have at least one team.** Teleport maps access via `teams_to_roles`, which requires team membership -- org ownership alone is not sufficient. Create a team (e.g. `infra`) and add all operators to it.

3. **The OAuth App must be authorized for the org.** Go to `https://github.com/organizations/waddle-social/settings/oauth_application_policy` and ensure the Teleport OAuth App is approved. Without this, the GitHub API will not return team data for that org.

4. **The `team` field in the connector must be the exact team slug** (lowercase, hyphenated). Teleport does NOT support wildcards -- `team: "*"` is treated as a literal string, not a glob. Use the actual slug, e.g. `team: infra`.

#### Teleport Configuration Details

Key config choices and why they matter:

| Setting | Value | Why |
|---|---|---|
| `web_listen_addr` | `0.0.0.0:3080` | HAProxy forwards port 443 to here via TCP/SNI passthrough |
| `tunnel_listen_addr` | `0.0.0.0:3024` | **Required.** Without this, the app service and SSH service cannot register with the proxy, causing "Unable to serve application requests" errors |
| `public_addr` | `teleport.waddle.social:443` | Must match the external URL users access; used for ACME cert requests and OAuth redirect validation |
| `acme.enabled` | `true` | Let's Encrypt TLS via TLS-ALPN-01 challenge; works through HAProxy because SNI passthrough preserves the ALPN negotiation |
| `insecure_skip_verify` | `true` (proxmox app) | Proxmox uses a self-signed cert on port 8006 |

#### Verify

1. Open `https://teleport.waddle.social` -- accept the community edition terms, click **GitHub**
2. Authenticate with GitHub -- you must be a member of `waddle-social/infra` team
3. The Teleport dashboard should show:
   - **proxmox** app (click Launch to access Proxmox UI)
   - **teleport** SSH node (click Connect, select login `deploy`)
4. Test app access: click Launch on `proxmox` -- the Proxmox web UI should load

#### Port Forwarding (Proxmox Host)

The Proxmox host and HAProxy VM share the same public IP. Port forwarding rules on the host route traffic to the correct internal VMs:

```bash
# On Proxmox host

# HTTP/HTTPS -> HAProxy VM (10.10.0.3)
iptables -t nat -A PREROUTING -p tcp -d <public_ip> --dport 443 -j DNAT --to-destination 10.10.0.3:443
iptables -t nat -A PREROUTING -p tcp -d <public_ip> --dport 80 -j DNAT --to-destination 10.10.0.3:80
iptables -A FORWARD -p tcp -d 10.10.0.3 --dport 443 -j ACCEPT
iptables -A FORWARD -p tcp -d 10.10.0.3 --dport 80 -j ACCEPT

# Teleport SSH proxy + reverse tunnel -> Teleport VM (10.10.0.2)
iptables -t nat -A PREROUTING -i vmbr0 -p tcp --dport 3023 -j DNAT --to-destination 10.10.0.2:3023
iptables -A FORWARD -i vmbr0 -o vmbr1 -p tcp --dport 3023 -j ACCEPT
iptables -t nat -A PREROUTING -i vmbr0 -p tcp --dport 3024 -j DNAT --to-destination 10.10.0.2:3024
iptables -A FORWARD -i vmbr0 -o vmbr1 -p tcp --dport 3024 -j ACCEPT

# NAT for return traffic
iptables -t nat -A POSTROUTING -s 10.10.0.0/24 -o vmbr0 -j MASQUERADE

# Persist
netfilter-persistent save
```

| Port | Destination | Purpose |
|---|---|---|
| 80 | 10.10.0.3 (HAProxy) | HTTP, forwarded to Cilium Gateway |
| 443 | 10.10.0.3 (HAProxy) | HTTPS, SNI-routed by HAProxy |
| 3023 | 10.10.0.2 (Teleport) | Teleport SSH proxy (`tsh ssh`, `tsh scp`) |
| 3024 | 10.10.0.2 (Teleport) | Teleport reverse tunnel (node/app registration) |

**Lockdown -- Remove temporary SSH access:**

After confirming Teleport works, remove the temporary SSH firewall rule:

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

> **Note:** Unlike HAProxy and Teleport, the Talos VMs deploy successfully via OpenTofu's `import_from` on ZFS datastores. The `initialization` block uses `datastore_id = "local"` (not `local-lvm`, which does not exist on this host).

What it creates:
- Downloads the Talos nocloud image from Image Factory
- Creates 3 VMs (IDs 110-112): 3 vCPU, 8 GB RAM, 10 GB disk each
- All on `vmbr1` with static IPs: 10.10.0.10, 10.10.0.11, 10.10.0.12
- CPU type `host` (passthrough) for x86-64-v2 support
- DNS set to 1.1.1.1 and 8.8.8.8

#### Bootstrap the Cluster

The bootstrap script and `talosctl` must be run from a machine on the `vmbr1` network (10.10.0.0/24) since Talos nodes have no public interface. Use the Teleport VM (10.10.0.2) as the jump host.

**1. Generate configs locally:**

```bash
./scripts/bootstrap-talos.sh
```

This generates configs in `talos/generated/` but will fail at the apply step (can't reach 10.10.0.x from your Mac). That's expected.

**2. Copy configs to the Teleport VM:**

```bash
tsh login --proxy=teleport.waddle.social --auth=github
tsh scp -r talos/generated/ deploy@teleport:/tmp/talos-configs/
```

**3. SSH into the Teleport VM and install tools:**

```bash
tsh ssh deploy@teleport

# Install talosctl
curl -sL https://talos.dev/install | sh

# Install kubectl
curl -LO "https://dl.k8s.io/release/$(curl -L -s https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl"
chmod +x kubectl && sudo mv kubectl /usr/local/bin/
```

> **Prerequisite:** The Teleport VM must use CPU type `host` (not the default `kvm64`) for `talosctl` to run. It requires x86-64-v2 microarchitecture support. If the VM was created manually, fix it on the Proxmox host:
> ```bash
> qm stop 101 && qm set 101 -cpu host && qm start 101
> ```
> Do the same for the HAProxy VM (100) for consistency.

**4. Apply configs and bootstrap:**

```bash
# Apply config to each node
talosctl apply-config --insecure --nodes 10.10.0.10 \
  --file /tmp/talos-configs/generated/controlplane.yaml \
  --config-patch @/tmp/talos-configs/generated/talos-cp1-patch.yaml

talosctl apply-config --insecure --nodes 10.10.0.11 \
  --file /tmp/talos-configs/generated/controlplane.yaml \
  --config-patch @/tmp/talos-configs/generated/talos-cp2-patch.yaml

talosctl apply-config --insecure --nodes 10.10.0.12 \
  --file /tmp/talos-configs/generated/controlplane.yaml \
  --config-patch @/tmp/talos-configs/generated/talos-cp3-patch.yaml

# Wait for nodes to initialize
sleep 30

# Bootstrap first node
export TALOSCONFIG=/tmp/talos-configs/generated/talosconfig
talosctl config endpoint 10.10.0.10
talosctl config node 10.10.0.10
talosctl bootstrap

# Wait for bootstrap
sleep 60

# Retrieve kubeconfig
talosctl kubeconfig /tmp/kubeconfig --force

# Verify
export KUBECONFIG=/tmp/kubeconfig
kubectl get nodes
# All 3 nodes in NotReady state (expected -- Cilium not yet installed)
```

**Verify:**

```bash
kubectl get nodes
# NAME        STATUS     ROLES           AGE   VERSION
# talos-cp1   NotReady   control-plane   ...   v1.35.0
# talos-cp2   NotReady   control-plane   ...   v1.35.0
# talos-cp3   NotReady   control-plane   ...   v1.35.0
```

> **Important:** The generated `talos/generated/` directory contains cluster secrets (PKI, tokens). It is gitignored. Back up `talos/generated/talosconfig` securely (e.g., in 1Password).

#### Teleport Port Forwarding for `tsh`

To use `tsh login`, `tsh ssh`, and `tsh scp` from your workstation, the Proxmox host must forward Teleport's SSH proxy port (3023) and reverse tunnel port (3024) to the Teleport VM. Add these rules on the Proxmox host:

```bash
# Teleport SSH proxy (tsh ssh/scp)
iptables -t nat -A PREROUTING -i vmbr0 -p tcp --dport 3023 -j DNAT --to-destination 10.10.0.2:3023
iptables -A FORWARD -i vmbr0 -o vmbr1 -p tcp --dport 3023 -j ACCEPT

# Teleport reverse tunnel (node/app registration)
iptables -t nat -A PREROUTING -i vmbr0 -p tcp --dport 3024 -j DNAT --to-destination 10.10.0.2:3024
iptables -A FORWARD -i vmbr0 -o vmbr1 -p tcp --dport 3024 -j ACCEPT

netfilter-persistent save
```

> **Note:** These rules point to 10.10.0.2 (Teleport VM), not 10.10.0.3 (HAProxy). Ports 3023/3024 are Teleport-specific protocols, not HTTP traffic.

Also ensure Teleport is listening on port 3023. If not, add to `/etc/teleport.yaml` under `proxy_service`:

```yaml
  listen_addr: 0.0.0.0:3023
  ssh_public_addr: teleport.waddle.social:3023
```

Then restart: `sudo systemctl restart teleport`

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

## Troubleshooting

### OpenTofu can't reach Proxmox API

The `proxmox_endpoint` must point to the Proxmox host's **public IP** during provisioning (not the internal `10.10.0.1`), since vmbr1 is not routable from your workstation. Set `proxmox_ssh_host` to the same public IP so the provider's SSH file uploads work.

### Cluster firewall locks out API access

If the cluster firewall is enabled with `input_policy = DROP` before the port 8006 allow rule exists, you'll lose API access. Recovery:

```bash
ssh root@<public_ip>
pvesh set /cluster/firewall/options --enable 0
```

The network module now includes a temporary rule allowing port 8006 from `operator_ip`.

### VM kernel panic on ZFS datastore (bpg/proxmox provider)

The bpg/proxmox provider's `import_from` and `file_id` disk import methods produce kernel panics on ZFS-backed datastores (`vmdata`). The workaround is to create VMs manually using `qm importdisk` on the Proxmox host and then import them into OpenTofu state:

```bash
# On Proxmox host
qm importdisk <vmid> /tmp/debian-12-generic-amd64.qcow2 vmdata
qm set <vmid> --scsi0 vmdata:vm-<vmid>-disk-0

# On workstation
./scripts/tofu.sh import module.<name>.proxmox_virtual_environment_vm.<resource> waddle-proxmox01/<vmid>
```

### SSH host key verification failures

VMs get new host keys when recreated. Remove stale entries:

```bash
ssh-keygen -R <ip>
```

### Cloud-init packages not installed

The Debian 12 cloud image ships cloud-init v22.4.2 which sometimes skips the `packages` section. The workaround is to install packages explicitly in `runcmd` using `apt-get install -y`.

### Teleport GitHub SSO: "Unable to log in"

Check `sudo journalctl -u teleport` on the Teleport VM for the real error. Common causes:

**"user does not belong to any teams configured in connector":**
- The GitHub user must be a member of a **team** in the configured org. Org ownership alone is not enough.
- The `team` field must be the exact team **slug** (lowercase). Wildcards (`*`) do NOT work -- Teleport uses exact string matching.
- The OAuth App must be **authorized** for the org at `https://github.com/organizations/<org>/settings/oauth_application_policy`.

**"acme/autocert: missing server name":**
- Harmless when connecting to Teleport via `127.0.0.1` or an IP address. The ACME TLS handler requires SNI. Connections through HAProxy (which preserves SNI) work correctly.

### Teleport: "Unable to serve application requests"

The proxy's reverse tunnel listener is not running. Add `tunnel_listen_addr: 0.0.0.0:3024` to the `proxy_service` section in `/etc/teleport.yaml` and restart Teleport. Without this, the app service cannot register with the proxy.

Verify it's listening: `sudo ss -tlnp | grep 3024`

### talosctl / kubectl: "command not found" or "x86-64-v2" error on Teleport VM

If `talosctl` prints "This program can only be run on AMD64 processors with v2 microarchitecture support", the VM's CPU type is set to `kvm64` (default for manually created VMs). Fix on the Proxmox host:

```bash
qm stop 101 && qm set 101 -cpu host && qm start 101
```

### Talos apply-config: "static hostname is already set in v1alpha1 config"

`talosctl gen config` (v1.12.4+) appends a `HostnameConfig` document (`auto: stable`) to the generated `controlplane.yaml`. This conflicts with `machine.network.hostname` in the per-node patches. The bootstrap script strips this document automatically with `sed`. If applying manually, remove the trailing `---` and `HostnameConfig` block from `controlplane.yaml` before applying.

### Talos apply-config: "storage 'local-lvm' does not exist"

The `initialization` block in the Talos VM resource defaults to `local-lvm` for the cloud-init drive. This host does not have `local-lvm`. Set `datastore_id = "local"` in the `initialization` block.

### tsh login: "dial tcp ...:3023: i/o timeout"

Port 3023 (Teleport SSH proxy) is not forwarded from the Proxmox host to the Teleport VM. Add DNAT rules for ports 3023 and 3024 pointing to 10.10.0.2 (see Phase 4, "Teleport Port Forwarding for tsh").

### Teleport SSH: "unknown user"

The default `access` role uses `{{internal.logins}}` which resolves to the GitHub username (e.g. `randax`). If that user doesn't exist on the target VM, SSH fails. Create a custom role with explicit `logins` (e.g. `deploy`, `root`) and assign it via the GitHub connector's `teams_to_roles`.
