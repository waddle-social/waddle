# Waddle Infrastructure - CDKTF

This workspace contains Infrastructure as Code (IaC) using [Terraform CDK](https://developer.hashicorp.com/terraform/cdktf) (CDKTF) for provisioning a Talos Kubernetes cluster on Proxmox.

## Overview

The infrastructure code provisions:
- Talos OS VMs on Proxmox (control plane and worker nodes)
- Talos cluster configuration and bootstrapping
- Kubernetes cluster initialization

## Prerequisites

- **Node.js** 20.9 or later
- **Terraform CLI** 1.0 or later
- **CDKTF CLI** (`bun install -g cdktf-cli`)
- **Proxmox VE** 9.1 with API access enabled
- **API Token** created in Proxmox (see setup below)

## Setup

### 1. Install Dependencies

```bash
cd infrastructure
bun install
bun run get  # Generate provider bindings
```

### 2. Configure Environment

Copy the example environment file and fill in your values:

```bash
cp .env.example .env
```

Edit `.env` with your Proxmox credentials. See `.env.example` for detailed documentation of all available options.

### 3. Create Proxmox API Token

1. Log into Proxmox web UI
2. Navigate to: **Datacenter → Permissions → API Tokens → Add**
3. Select a user (e.g., `root@pam`) and create a token
4. Copy the token ID and secret (shown only once!)
5. Set `PROXMOX_VE_API_TOKEN` to: `user@realm!tokenid=secret`

## Provider Configuration

This project uses **generated provider bindings** via `cdktf get`, which creates TypeScript types under `.gen/providers/`. Provider versions are pinned in `cdktf.json`.

### Proxmox Provider (bpg/proxmox)

- **Version:** ~> 0.78.0
- **Bindings:** Generated via `cdktf get` → `.gen/providers/proxmox/`
- **Documentation:** https://registry.terraform.io/providers/bpg/proxmox/latest/docs

The Proxmox provider enables:
- VM creation and management
- Cloud-init configuration
- Storage management
- Network configuration

**Authentication:** Proxmox credentials are exposed as Terraform variables (`proxmox_endpoint`, `proxmox_api_token`) which can be set via:
- CLI flags: `-var="proxmox_endpoint=..."` 
- `terraform.tfvars` file
- `TF_VAR_*` environment variables
- Node.js environment variables (as defaults)

### Talos Provider (siderolabs/talos)

- **Version:** ~> 0.9.0
- **Bindings:** Generated via `cdktf get` → `.gen/providers/talos/`
- **Documentation:** https://registry.terraform.io/providers/siderolabs/talos/latest/docs

The Talos provider enables:
- Machine secrets generation
- Machine configuration generation
- Cluster bootstrapping
- Kubeconfig and talosconfig retrieval

The provider itself requires minimal configuration (only `imageFactoryUrl` is optional). Connection details for Talos clusters are specified at the resource level, not at the provider level.

## Architecture

### Directory Structure

```
infrastructure/
├── main.ts                 # Application entry point
├── lib/
│   ├── constructs/         # Reusable CDKTF constructs for infrastructure components
│   │   ├── talos-image.ts  # Talos OS image download management
│   │   ├── talos-vm.ts     # Talos VM provisioning on Proxmox
│   │   └── index.ts
│   ├── providers/          # Provider configuration constructs
│   │   ├── proxmox-provider.ts
│   │   ├── talos-provider.ts
│   │   └── index.ts
│   └── stacks/             # Stack definitions
│       ├── proxmox-stack.ts
│       └── index.ts
├── cdktf.json              # CDKTF configuration
├── package.json            # Dependencies
└── .env.example            # Environment template
```

### Stack Structure

The `ProxmoxStack` is the primary stack containing:
- Proxmox provider configuration
- Talos provider configuration
- (Future) VM provisioning resources
- (Future) Cluster bootstrap resources

### Construct Pattern

Reusable constructs encapsulate provider and resource configurations:
- `ProxmoxProviderConstruct`: Proxmox provider setup with env var support
- `TalosProviderConstruct`: Talos provider setup
- (Future) VM constructs for control plane and workers

## Usage

### Generate Terraform Configuration

```bash
bun run synth
```

This generates Terraform JSON in `cdktf.out/`.

### Deploy Infrastructure

```bash
bun run deploy
```

Apply the infrastructure changes to Proxmox.

### Destroy Infrastructure

```bash
bun run destroy
```

Tear down all provisioned resources.

### Build TypeScript

```bash
bun run build
```

Compile TypeScript to JavaScript.

### Run Tests

```bash
bun test
```

Execute unit tests with Jest.

### Watch Mode

```bash
bun run watch
```

Watch for TypeScript changes and recompile.

## Environment Variables

See `.env.example` for complete documentation. Summary:

### Terraform Variables (set at apply time)

| Variable | Required | Description |
|----------|----------|-------------|
| `proxmox_endpoint` | Yes* | Proxmox API URL |
| `proxmox_api_token` | Yes* | API token (sensitive) |

*Required unless defaults are provided via Node.js env vars.

### Node.js Environment Variables (used as defaults)

| Variable | Required | Description |
|----------|----------|-------------|
| `PROXMOX_VE_ENDPOINT` | No | Default for `proxmox_endpoint` |
| `PROXMOX_VE_API_TOKEN` | No | Default for `proxmox_api_token` |
| `PROXMOX_VE_INSECURE` | No | Skip TLS verify |
| `PROXMOX_VE_SSH_USERNAME` | No | SSH username |
| `PROXMOX_VE_SSH_PASSWORD` | No | SSH password |
| `PROXMOX_VE_SSH_AGENT` | No | Use SSH agent |
| `ENVIRONMENT` | No | Environment tag |

## Security Notes

- **Never commit** `.env` files, secrets, or state files
- **Use API tokens** instead of passwords where possible
- **Enable TLS verification** in production (`PROXMOX_VE_INSECURE=false`)
- **Use secrets management** (HashiCorp Vault, etc.) for CI/CD
- The `.gitignore` already excludes sensitive files

## Phase 3: Talos VM Provisioning

This phase provisions Talos Linux VMs on Proxmox for a Kubernetes cluster.

### Architecture

Supported topologies:
- **3 control planes, 0 workers** (default) - HA cluster with workloads on control plane nodes
- **1 control plane, N workers** - Small cluster with dedicated worker nodes
- **3 control planes, N workers** - Production HA with dedicated workers

VM specifications (configurable via env vars):
- **Control planes**: 4 cores, 8GB RAM, 50GB disk (default)
- **Workers**: 2 cores, 4GB RAM, 50GB disk (default)
- **CPU type**: x86-64-v2-AES (minimum for Talos)

Network modes:
- **Static IP**: Sequential IPs from `TALOS_NODE_IP_START` (only /24 networks supported)
- **DHCP**: VMs obtain addresses from DHCP server

**Topology labels**: Region and zone labels for Kubernetes scheduling

### Image Management

Talos images are downloaded from Image Factory (`factory.talos.dev`) using the nocloud platform format. The default schematic ID produces a vanilla Talos image suitable for most deployments.

### Required Environment Variables for VM Provisioning

| Variable | Required | Description |
|----------|----------|-------------|
| `PROXMOX_NODE_NAME` | Yes | Proxmox node name (e.g., 'pve') |
| `TALOS_CLUSTER_ENDPOINT` | Yes | Cluster API endpoint (e.g., 'https://192.168.1.100:6443') |

**Static IP mode** (set both or neither for DHCP):

| Variable | Required | Description |
|----------|----------|-------------|
| `TALOS_NODE_IP_PREFIX` | For static | IP prefix - three octets only (e.g., '192.168.1') |
| `TALOS_NODE_GATEWAY` | For static | Network gateway |

**Storage configuration**:

| Variable | Default | Description |
|----------|---------|-------------|
| `PROXMOX_STORAGE_ID` | `local-lvm` | Storage for VM disks (may be Ceph, LVM, ZFS) |
| `PROXMOX_IMAGE_STORAGE_ID` | `local` | Storage for ISO/images (must support 'iso' content) |

See `.env.example` for complete list with defaults and optional variables.

### Deploying VMs

1. Copy `.env.example` to `.env` and configure required variables
2. Run `bun run get` to generate provider bindings (first time only)
3. Run `bun run synth` to generate Terraform configuration
4. Run `bun run deploy` to provision VMs on Proxmox
5. Verify VMs in Proxmox web UI
6. Check outputs: `cdktf output` to see VM IPs and cluster info

### VM Outputs

After deployment, the following outputs are available:

| Output | Description |
|--------|-------------|
| `control_plane_ips` | Array of control plane IP addresses (undefined if DHCP) |
| `control_plane_vm_ids` | Array of Proxmox VM IDs |
| `worker_ips` | Array of worker IP addresses (if workers configured) |
| `worker_vm_ids` | Array of worker Proxmox VM IDs (if workers configured) |
| `cluster_endpoint` | Kubernetes API endpoint URL |
| `talos_version` | Deployed Talos version |
| `cluster_name` | Kubernetes cluster name |
| `network_mode` | Network configuration mode (static or dhcp) |

### Network Configuration

**Static IP mode**:
- Set `TALOS_NODE_IP_PREFIX` and `TALOS_NODE_GATEWAY`
- VMs receive sequential IPs: control planes first, then workers
- **Example**: If `ipPrefix=192.168.1`, `ipStart=101`, 3 CPs, 2 workers:
  - Control planes: `.101`, `.102`, `.103`
  - Workers: `.104`, `.105`
- **Important**: Only /24 networks are supported. `TALOS_NODE_IP_PREFIX` must be exactly three octets (e.g., `192.168.1`)

**DHCP mode**:
- Leave `TALOS_NODE_IP_PREFIX` and `TALOS_NODE_GATEWAY` unset (or empty)
- VMs obtain IPs from your DHCP server
- Configure DHCP reservations based on VM MAC addresses (visible in Proxmox after creation)

**Common settings**:
- **Bridge**: Default `vmbr0`, change via `PROXMOX_NETWORK_BRIDGE`

### Topology Labels

Labels enable Kubernetes zone-aware scheduling and volume topology:

| Label | Purpose |
|-------|---------|
| `topology.kubernetes.io/region` | Logical region (default: 'proxmox') |
| `topology.kubernetes.io/zone` | Availability zone (default: 'zone-1') |
| `node-role.kubernetes.io/control-plane` | Marks control plane nodes |
| `node-role.kubernetes.io/worker` | Marks worker nodes |

Labels are stored in VM description and will be applied to Kubernetes nodes in Phase 4.

## Troubleshooting

### Missing Environment Variables

```
Configuration error: Missing required environment variable: PROXMOX_VE_ENDPOINT
```

Ensure `.env` file exists and contains required variables. See `.env.example`.

### Invalid API Token

```
Error: 401 Unauthorized
```

Verify your API token:
1. Check the token format: `user@realm!tokenid=secret`
2. Ensure the token hasn't expired
3. Verify token permissions in Proxmox

### TLS Certificate Errors

```
Error: certificate verify failed
```

Options:
1. (Recommended) Add Proxmox CA to your trust store
2. (Development only) Set `PROXMOX_VE_INSECURE=true`

### Network Connectivity

```
Error: connection refused
```

Verify:
1. Proxmox API endpoint is reachable
2. Firewall allows port 8006 (or custom port)
3. VPN is connected if required

### Enable Debug Logging

```bash
TF_LOG=DEBUG bun run deploy
```

### VM Provisioning Issues

**Image download fails:**
- Check Proxmox storage permissions and internet connectivity
- Verify the storage supports ISO/image content type

**VM creation fails:**
- Verify `PROXMOX_NODE_NAME` matches actual node name (case-sensitive)
- Check Proxmox API token has required permissions

**IP conflicts:**
- Ensure `TALOS_NODE_IP_START` range is available on network
- Verify no existing VMs use the same IP addresses

**Storage full:**
- Check Proxmox storage capacity (50GB per VM + image)
- Talos image is ~150MB, downloaded once per version

## Provider Versions

Provider versions are pinned in `cdktf.json`:

- **bpg/proxmox:** ~> 0.78.0
- **siderolabs/talos:** ~> 0.9.0

Talos v1.11.5 is the latest stable version as of deployment. Update `TALOS_VERSION` env var to use different versions.

## Phase 4: Talos Cluster Bootstrap

This phase bootstraps the Kubernetes cluster on provisioned Talos VMs.

**IMPORTANT: Phase 4 requires static IP configuration.** DHCP mode is currently supported only for VM provisioning (Phase 3). IP discovery for DHCP-based clusters will be implemented in a future release. To enable Phase 4, you must configure static IPs:
- Set `TALOS_NODE_IP_PREFIX` (e.g., `192.168.1`)
- Set `TALOS_NODE_GATEWAY` (e.g., `192.168.1.1`)

If DHCP mode is detected (no static IP configuration), Phase 4 will be skipped with a warning message, and VMs will be provisioned without cluster bootstrap.

### Bootstrap Workflow

1. **Generate Cluster Secrets**: Create certificates, tokens, and encryption keys
2. **Generate Machine Configurations**: Create control plane and worker configs with CNI disabled
3. **Apply Configurations**: Push configs to each node via Talos API
4. **Bootstrap Cluster**: Initialize etcd and Kubernetes control plane on first CP node
5. **Generate Kubeconfig**: Create kubeconfig for kubectl access

### Architecture

**CNI Configuration**: Set to `none` by default. Cilium will be installed in Phase 6 via Flux.

**Node Labels**: Topology labels from Phase 3 VM metadata are injected into Kubernetes nodes via `machine.nodeLabels` in the Talos machine configuration. The following labels are applied:
- `topology.kubernetes.io/region`
- `topology.kubernetes.io/zone`
- `node-role.kubernetes.io/control-plane` or `node-role.kubernetes.io/worker`

These labels are set in the Talos machine config patches during bootstrap, ensuring nodes are correctly labeled when they join the cluster.

**Control Plane Scheduling**: 
- Control-plane-only clusters (no workers): Workloads allowed on control planes
- Clusters with workers: Control planes tainted to prevent workload scheduling
- Override with `TALOS_ALLOW_SCHEDULING_ON_CONTROL_PLANES`

### Environment Variables

All Phase 4 variables are optional with sensible defaults:

| Variable | Default | Description |
|----------|---------|-------------|
| `TALOS_KUBERNETES_VERSION` | Talos default | Kubernetes version (e.g., 'v1.31.0') |
| `TALOS_CLUSTER_DOMAIN` | `cluster.local` | Cluster domain for services |
| `TALOS_CLUSTER_NETWORK` | `10.244.0.0/16` | Pod network CIDR |
| `TALOS_SERVICE_NETWORK` | `10.96.0.0/12` | Service network CIDR |
| `TALOS_CNI` | `none` | CNI name ('none' for external CNI) |
| `TALOS_INSTALL_DISK` | `/dev/sda` | Disk path for Talos installation |
| `TALOS_ALLOW_SCHEDULING_ON_CONTROL_PLANES` | Auto | Allow workloads on control planes |

See `.env.example` for detailed documentation.

### Bootstrapping the Cluster

1. **Ensure VMs are deployed** (Phase 3 complete):
   ```bash
   bun run deploy
   ```

2. **Verify VMs are running** in Proxmox web UI

3. **Bootstrap will happen automatically** during deployment if Phase 3 VMs exist

4. **Check outputs** for kubeconfig:
   ```bash
   cdktf output
   ```

5. **Extract kubeconfig** (base64 encoded in output):
   ```bash
   cdktf output -raw kubeconfig_raw | base64 -d > ~/.kube/config
   ```

6. **Verify cluster access**:
   ```bash
   kubectl get nodes
   kubectl get pods -A
   ```

### Outputs

After bootstrap, the following outputs are available:

| Output | Description |
|--------|-------------|
| `kubeconfig_raw` | Base64-encoded kubeconfig (sensitive) |
| `talosconfig_raw` | Base64-encoded talosconfig (sensitive) |
| `kubernetes_version` | Deployed Kubernetes version |
| `cluster_ready` | Cluster bootstrap status |

### Accessing the Cluster

**With kubectl**:
```bash
# Extract kubeconfig
cdktf output -raw kubeconfig_raw | base64 -d > ~/.kube/config

# Verify access
kubectl get nodes
kubectl cluster-info
```

**With talosctl** (for Talos API access):
```bash
# Extract talosconfig
cdktf output -raw talosconfig_raw | base64 -d > ~/.talos/config

# Set context
export TALOSCONFIG=~/.talos/config

# Verify access
talosctl -n <node-ip> version
talosctl -n <node-ip> dashboard
```

### Network Configuration

**Pod Network**: Default `10.244.0.0/16` (managed by Cilium in Phase 6)
**Service Network**: Default `10.96.0.0/12` (Kubernetes services)
**Cluster Domain**: Default `cluster.local` (DNS suffix for services)

Ensure these ranges don't conflict with your existing network infrastructure.

### Troubleshooting

**Bootstrap fails with connection timeout**:
- Verify VMs are running and network is configured
- Check firewall rules allow Talos API port 50000
- Verify `TALOS_CLUSTER_ENDPOINT` is reachable from deployment machine

**Nodes not joining cluster**:
- Check Talos logs: `talosctl -n <node-ip> logs`
- Verify machine configs were applied: `talosctl -n <node-ip> get machineconfig`
- Check etcd health: `talosctl -n <cp-ip> etcd members`

**Kubeconfig not working**:
- Verify cluster endpoint is reachable
- Check certificate validity
- Ensure Kubernetes API server is running: `talosctl -n <cp-ip> service kubelet status`

**CNI not installed warning**:
- Expected behavior - CNI is set to 'none'
- Cilium will be installed in Phase 6
- Pods will remain in Pending state until CNI is installed

### Security Notes

- **Kubeconfig and talosconfig are sensitive**: Never commit to version control
- **Rotate credentials regularly**: Regenerate secrets periodically
- **Use RBAC**: Configure Kubernetes RBAC for access control
- **Teleport integration**: Phase 5 adds secure access via Teleport
- **Store secrets securely**: Use secrets management tools (Vault, etc.) for production

## Phase 5: Teleport Secure Access

This phase deploys Teleport for zero-trust access to Proxmox and Kubernetes infrastructure.

### Architecture

**Teleport Components:**
- **Auth Service**: Authentication, authorization, and audit logging
- **Proxy Service**: Public-facing gateway (single public IP)
- **Agents**: Connect infrastructure via reverse tunnels

**Deployment:**
- Teleport VM on Proxmox running Auth + Proxy services
- Teleport Kube Agent in Kubernetes (deployed after Cilium in Phase 6)
- Application Access for Proxmox web UI
- SSH Access for Proxmox host

**Security Model:**
- Only Teleport Proxy exposed on public IP (ports 443, 3024)
- All other services accessed through Teleport reverse tunnels
- No need to expose Proxmox or Kubernetes API publicly
- Comprehensive audit logging and session recording

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `TELEPORT_ENABLED` | Yes | Set to 'true' to enable Teleport VM provisioning |
| `TELEPORT_DOMAIN` | Yes | Public FQDN for Teleport (e.g., 'teleport.waddle.social') |
| `TELEPORT_LETSENCRYPT_EMAIL` | Yes | Email for Let's Encrypt ACME registration |
| `TELEPORT_IP_ADDRESS` | Yes | Static IP for Teleport VM |
| `TELEPORT_GATEWAY` | Yes | Network gateway |
| `TELEPORT_SSH_KEYS` | Yes | Comma-separated SSH public keys for initial access |
| `TELEPORT_NODE_NAME` | No | Proxmox node (defaults to `PROXMOX_NODE_NAME`) |
| `TELEPORT_STORAGE_ID` | No | Storage for VM disk (defaults to `PROXMOX_STORAGE_ID`) |
| `TELEPORT_NETWORK_BRIDGE` | No | Network bridge (defaults to `PROXMOX_NETWORK_BRIDGE`) |
| `TELEPORT_VM_NAME` | No | VM name in Proxmox (default: 'teleport') |
| `TELEPORT_CORES` | No | CPU cores (default: 2, minimum: 2) |
| `TELEPORT_MEMORY` | No | Memory in MB (default: 4096, minimum: 2048) |
| `TELEPORT_DISK_SIZE` | No | Disk size in GB (default: 50, minimum: 20) |
| `TELEPORT_VERSION` | No | Teleport version (default: 'latest') |
| `TELEPORT_NETMASK` | No | Network CIDR suffix (default: 24) |

See `.env.example` for detailed documentation and examples.

### Prerequisites

1. **DNS Configuration**: Create an A record pointing `TELEPORT_DOMAIN` to your public IP
2. **Firewall Rules**: Forward ports 443 and 3024 to `TELEPORT_IP_ADDRESS`
3. **SSH Keys**: Generate SSH key pair for initial VM access

### Deploying Teleport VM

1. **Configure environment variables** in `.env`:
   ```bash
   TELEPORT_ENABLED=true
   TELEPORT_DOMAIN=teleport.waddle.social
   TELEPORT_LETSENCRYPT_EMAIL=admin@waddle.social
   TELEPORT_IP_ADDRESS=192.168.1.100
   TELEPORT_GATEWAY=192.168.1.1
   TELEPORT_SSH_KEYS="ssh-ed25519 AAAA..."
   ```

2. **Deploy infrastructure**:
   ```bash
   bun run synth
   bun run deploy
   ```

3. **Verify deployment**:
   ```bash
   cdktf output
   ```

   Expected outputs:
   - `teleport_vm_id`: Proxmox VM ID
   - `teleport_ip`: VM IP address
   - `teleport_domain`: Public domain
   - `teleport_web_ui`: Web UI URL

### First-Time Setup

1. **SSH to Teleport VM**:
   ```bash
   ssh admin@192.168.1.100
   ```

2. **Install Teleport** (see `docs/teleport-setup.md` for full instructions):
   ```bash
   curl https://apt.releases.teleport.dev/gpg -o /usr/share/keyrings/teleport-archive-keyring.asc
   echo "deb [signed-by=/usr/share/keyrings/teleport-archive-keyring.asc] https://apt.releases.teleport.dev/debian bookworm stable/v17" | sudo tee /etc/apt/sources.list.d/teleport.list
   sudo apt-get update && sudo apt-get install -y teleport
   ```

3. **Configure Teleport** and start the service

4. **Create first admin user**:
   ```bash
   sudo tctl users add admin --roles=editor,access --logins=root,admin
   ```

5. **Complete user setup**:
   - Open the invite URL in a browser
   - Set password and configure MFA
   - Login to Teleport web UI

### Outputs

After deployment, the following outputs are available:

| Output | Description |
|--------|-------------|
| `teleport_vm_id` | Proxmox VM ID |
| `teleport_ip` | Teleport VM IP address |
| `teleport_domain` | Public domain for Teleport |
| `teleport_web_ui` | Web UI URL (https://{domain}) |
| `teleport_letsencrypt_email` | ACME email for certificates |
| `teleport_version` | Teleport version to install |

### Accessing Infrastructure via Teleport

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
tsh login --proxy=teleport.waddle.social:443 --user=<username>
```

**Access Proxmox:**
```bash
# SSH
tsh ssh root@pve

# Web UI
tsh apps login proxmox-web
tsh apps open proxmox-web
```

**Access Kubernetes (after Phase 6):**
```bash
tsh kube login waddle-cluster
kubectl get nodes
```

### Kubernetes Integration

**Note:** Kubernetes integration is configured after Cilium CNI is installed (Phase 6).

See `infrastructure-k8s/teleport/README.md` for Kubernetes agent deployment.

### Security Features

- **Multi-Factor Authentication**: Required for all users
- **Session Recording**: All SSH and kubectl sessions recorded
- **Audit Logging**: Comprehensive logs of all access
- **RBAC**: Fine-grained role-based access control
- **Zero Trust**: Certificate-based authentication
- **Reverse Tunnels**: No need to expose internal services

### Troubleshooting

**Cannot access web UI:**
- Verify DNS: `dig teleport.waddle.social +short`
- Check firewall port forwarding
- Verify Teleport service: `sudo systemctl status teleport`

**Certificate errors:**
- Check Let's Encrypt rate limits
- Verify domain points to correct IP
- Review Teleport logs: `sudo journalctl -u teleport -n 100`

**SSH access denied:**
- Verify user roles: `sudo tctl get users/<username>`
- Check node labels and role permissions

See `docs/teleport-setup.md` for comprehensive troubleshooting.

## Next Steps

- **Configure Teleport**: Complete Proxmox integration and create team users (see `docs/teleport-setup.md`)
- **Phase 6:** Install Cilium CNI with Gateway API support
- Deploy Teleport Kube Agent for Kubernetes access
- Bootstrap Flux for GitOps management
