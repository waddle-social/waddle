# Proxmox CSI Driver for Talos Kubernetes

This directory contains configuration for deploying the Proxmox CSI driver to provide persistent storage for Kubernetes workloads using Proxmox storage backends.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [Proxmox User Setup](#proxmox-user-setup)
- [Manual Installation (Phase 7)](#manual-installation-phase-7)
- [StorageClass Configuration](#storageclass-configuration)
- [Verification](#verification)
- [Troubleshooting](#troubleshooting)
- [Limitations](#limitations)
- [Files in This Directory](#files-in-this-directory)
- [References](#references)

## Architecture Overview

**Proxmox CSI Components:**
- **CSI Controller:** Manages volume provisioning, attaching, and deletion via Proxmox API
- **CSI Node Plugin:** Runs on every node as DaemonSet, handles volume mounting/unmounting

**Volume Lifecycle:**
```
┌─────────────────────────────────────────────────────────────────┐
│                     Volume Provisioning Flow                     │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. PVC Created                                                  │
│       ↓                                                          │
│  2. CSI Controller receives CreateVolume request                 │
│       ↓                                                          │
│  3. Controller calls Proxmox API (VM.Config.Disk)               │
│       ↓                                                          │
│  4. Proxmox allocates disk on storage backend                    │
│       ↓                                                          │
│  5. PV created and bound to PVC                                  │
│       ↓                                                          │
│  6. Pod scheduled → CSI Node Plugin attaches volume              │
│       ↓                                                          │
│  7. Volume mounted to pod container                              │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Integration with Proxmox:**
```
┌─────────────────────────────────────────────────────────────────┐
│                     Talos Kubernetes Cluster                     │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    csi-proxmox namespace                  │   │
│  │  ┌─────────────────┐    ┌─────────────────┐              │   │
│  │  │ CSI Controller  │    │  CSI Node (DS)  │              │   │
│  │  │   (Deployment)  │    │   per node      │              │   │
│  │  └────────┬────────┘    └────────┬────────┘              │   │
│  │           │                      │                        │   │
│  └───────────┼──────────────────────┼────────────────────────┘   │
│              │                      │                            │
│              ↓                      ↓                            │
│         Proxmox API ─────────── Volume Attach                    │
│         (port 8006)            (VM disk)                         │
│              │                                                   │
│              ↓                                                   │
│    ┌─────────────────────────────────────────────┐              │
│    │         Proxmox Storage Backend             │              │
│    │  (local-lvm, local-zfs, ceph-pool, etc.)   │              │
│    └─────────────────────────────────────────────┘              │
└─────────────────────────────────────────────────────────────────┘
```

## Prerequisites

Before installing the Proxmox CSI driver, ensure:

1. **Talos cluster with Cilium CNI** (Phase 6 complete)
   - Nodes should be in `Ready` state
   - Verify: `kubectl get nodes`

2. **Proxmox 9.1 with accessible API**
   - API endpoint reachable from cluster
   - Verify: `curl -k https://<proxmox-host>:8006/api2/json/version`

3. **Proxmox storage backend configured**
   - Check available storage: `pvesm status` (on Proxmox host)
   - Note the storage ID for CSI (e.g., `local-lvm`, `local-zfs`)

4. **kubectl and Helm 3.x installed**
   ```bash
   kubectl version
   helm version
   ```

## Proxmox User Setup

The CSI driver requires a dedicated Proxmox user with specific permissions for security and auditability.

### Option 1: CLI Setup (Recommended)

SSH to your Proxmox host and run:

```bash
# Create CSI role with required permissions
pveum role add CSI -privs "VM.Audit VM.Config.Disk Datastore.Allocate Datastore.AllocateSpace Datastore.Audit"

# Create dedicated user
pveum user add kubernetes-csi@pve

# Assign role to user (root path for all resources)
pveum aclmod / -user kubernetes-csi@pve -role CSI

# Create API token (WITHOUT privilege separation)
pveum user token add kubernetes-csi@pve csi -privsep 0
```

**IMPORTANT:** Save the token output! It's shown only once:
```
┌──────────────┬──────────────────────────────────────────────────────────────┐
│ key          │ value                                                        │
╞══════════════╪══════════════════════════════════════════════════════════════╡
│ full-tokenid │ kubernetes-csi@pve!csi                                       │
├──────────────┼──────────────────────────────────────────────────────────────┤
│ info         │ {"privsep":"0"}                                              │
├──────────────┼──────────────────────────────────────────────────────────────┤
│ value        │ xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx                         │
└──────────────┴──────────────────────────────────────────────────────────────┘
```

### Option 2: Web UI Setup

1. **Create Role:**
   - Navigate to: Datacenter → Permissions → Roles → Create
   - Name: `CSI`
   - Privileges: `VM.Audit`, `VM.Config.Disk`, `Datastore.Allocate`, `Datastore.AllocateSpace`, `Datastore.Audit`

2. **Create User:**
   - Navigate to: Datacenter → Permissions → Users → Add
   - User name: `kubernetes-csi`
   - Realm: `pve`
   - Leave password empty (API token only)

3. **Create API Token:**
   - Navigate to: Datacenter → Permissions → API Tokens → Add
   - User: `kubernetes-csi@pve`
   - Token ID: `csi`
   - **Uncheck** "Privilege Separation"
   - Copy the token value (shown only once!)

4. **Assign Role:**
   - Navigate to: Datacenter → Permissions → Add → User Permission
   - Path: `/`
   - User: `kubernetes-csi@pve`
   - Role: `CSI`

### Permission Explanation

| Permission | Purpose |
|------------|---------|
| `VM.Audit` | Read VM configuration to verify disk attachment |
| `VM.Config.Disk` | Attach/detach disks to VMs |
| `Datastore.Allocate` | Create new volumes on storage |
| `Datastore.AllocateSpace` | Allocate disk space |
| `Datastore.Audit` | Read storage status and configuration |

### Verify Setup

```bash
# List users and verify kubernetes-csi@pve exists
pveum user list | grep kubernetes-csi

# List ACLs and verify role assignment
pveum acl list | grep kubernetes-csi

# Test API token (replace with your values)
curl -k -H "Authorization: PVEAPIToken=kubernetes-csi@pve!csi=<token>" \
  https://<proxmox-host>:8006/api2/json/version
```

## Manual Installation (Phase 7)

### Step 1: Create Namespace

```bash
kubectl create namespace csi-proxmox
```

### Step 2: Create Credentials Secret

**PREREQUISITE:** The namespace `csi-proxmox` must exist before creating the Secret:
```bash
kubectl create namespace csi-proxmox
```

Create a config file with your Proxmox credentials:

```bash
cat > proxmox-csi-config.yaml <<EOF
clusters:
  - url: https://proxmox.waddle.social:8006/api2/json
    insecure: false
    token_id: "kubernetes-csi@pve!csi"
    token_secret: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
    region: proxmox
EOF
```

**Note:** The values above should be derived from the `PROXMOX_CSI_*` variables in `infrastructure/.env.example` for consistency with VM provisioning configuration.

Create the Kubernetes secret:

```bash
kubectl create secret generic proxmox-csi-credentials \
  --from-file=config.yaml=proxmox-csi-config.yaml \
  -n csi-proxmox
```

**Security:** Delete the local config file after creating the secret:
```bash
rm proxmox-csi-config.yaml
```

### Step 3: Add Helm Repository

```bash
helm repo add proxmox-csi https://sergelogvinov.github.io/proxmox-csi-plugin
helm repo update
```

### Step 4: Install CSI Driver

```bash
cd infrastructure-k8s/storage

helm install proxmox-csi proxmox-csi/proxmox-csi-plugin \
  --version 0.13.0 \
  --namespace csi-proxmox \
  --values helm-values.yaml
```

### Step 5: Verify Installation

```bash
# Check CSI controller pod
kubectl get pods -n csi-proxmox -l app=proxmox-csi-plugin-controller

# Check CSI node pods (one per node)
kubectl get pods -n csi-proxmox -l app=proxmox-csi-plugin-node

# Check StorageClass
kubectl get storageclass proxmox-csi

# Check CSI driver registration
kubectl get csidrivers
```

Expected output:
```
NAME                         ATTACHREQUIRED   PODINFOONMOUNT   STORAGECAPACITY   ...
csi.proxmox.sinextra.dev    true             true             false             ...
```

## StorageClass Configuration

### Decision Guide: Helm-Managed vs Standalone StorageClass

**IMPORTANT:** You must choose ONE method for creating the StorageClass. Do NOT use both simultaneously to avoid duplicate or conflicting definitions.

| Option | When to Use | Configuration |
|--------|-------------|---------------|
| **Helm-managed** (recommended) | Default setup, managed lifecycle | Keep `storageClass` array populated in `helm-values.yaml` |
| **Standalone manifest** | Custom requirements, separate lifecycle | Set `storageClass: []` in `helm-values.yaml`, apply `storageclass.yaml` |

**To use Helm-managed StorageClass (default):**
- Ensure `helm-values.yaml` has the `storageClass` array configured
- Do NOT apply `storageclass.yaml` manifest
- StorageClass lifecycle is managed by Helm

**To use standalone StorageClass:**
1. Edit `helm-values.yaml` and set `storageClass: []`
2. Redeploy the Helm chart
3. Apply the standalone manifest: `kubectl apply -f storageclass.yaml`

**Switching between options:**
1. Delete existing StorageClass: `kubectl delete storageclass proxmox-csi`
2. Update `helm-values.yaml` accordingly
3. Redeploy Helm chart or apply standalone manifest

### Default StorageClass Parameters

| Parameter | Value | Description |
|-----------|-------|-------------|
| `storage` | `local-lvm` | Proxmox storage ID (from `PROXMOX_CSI_STORAGE_ID`) |
| `cache` | `writethrough` | Disk cache mode (balanced performance/safety) |
| `reclaimPolicy` | `Delete` | Volume deleted when PVC is deleted |
| `volumeBindingMode` | `WaitForFirstConsumer` | Delay binding until pod is scheduled |
| `allowVolumeExpansion` | `false` | Not supported in v0.13.0 |

### Cache Modes

| Mode | Performance | Data Safety | Use Case |
|------|-------------|-------------|----------|
| `directsync` | Slowest | Safest | Databases, critical data |
| `writethrough` | Balanced | Good | General workloads (default) |
| `writeback` | Fastest | Risky | Temporary data, caches |
| `none` | Variable | Variable | Let storage decide |

### Creating Additional StorageClasses

For different storage backends or performance tiers:

```yaml
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: proxmox-csi-fast
provisioner: csi.proxmox.sinextra.dev
parameters:
  storage: local-nvme  # Different storage backend
  cache: writeback
  ssd: "true"
reclaimPolicy: Delete
volumeBindingMode: WaitForFirstConsumer
```

## Verification

### Quick Test (PVC)

```bash
# Apply namespace first (required)
kubectl apply -f verification/namespace.yaml

# Apply test manifest
kubectl apply -f verification/test-pvc.yaml

# Check PVC status (should be Bound)
kubectl get pvc -n csi-test

# Check PV created
kubectl get pv

# Check test pod
kubectl get pod -n csi-test

# Verify data written
kubectl exec -n csi-test test-pod -- cat /data/test.txt

# Cleanup
kubectl delete -f verification/test-pvc.yaml
```

### StatefulSet Test

```bash
# Apply namespace first (if not already created)
kubectl apply -f verification/namespace.yaml

# Apply StatefulSet test
kubectl apply -f verification/test-statefulset.yaml

# Check PVCs created (one per replica)
kubectl get pvc -n csi-test

# Check pods
kubectl get pods -n csi-test -l app=test-statefulset

# Verify data persistence (delete and recreate pod)
kubectl delete pod test-statefulset-0 -n csi-test
kubectl exec -n csi-test test-statefulset-0 -- cat /data/hostname.txt

# Cleanup
kubectl delete -f verification/test-statefulset.yaml
kubectl delete pvc -n csi-test -l app=test-statefulset  # PVCs may need manual deletion
```

### Verify in Proxmox

After creating a PVC, verify the volume in Proxmox:
1. Open Proxmox web UI
2. Navigate to the VM running your pod
3. Check Hardware → Hard Disk
4. You should see a new disk attached (e.g., `scsi1`)

## Troubleshooting

### CSI Controller Pod CrashLoopBackOff

**Check logs:**
```bash
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-controller --tail=100
```

**Common causes:**
- Invalid credentials - verify Secret contents
- API endpoint unreachable - check network connectivity
- SSL certificate issues - set `insecure: true` for self-signed certs (not recommended)

**Verify Secret:**
```bash
kubectl get secret proxmox-csi-credentials -n csi-proxmox -o jsonpath='{.data.config\.yaml}' | base64 -d
```

### PVC Stuck in Pending

**Check events:**
```bash
kubectl describe pvc <pvc-name> -n <namespace>
```

**Common causes:**
- StorageClass not found - verify `storageClassName` matches
- Storage backend full - check Proxmox storage status
- Permission denied - verify CSI user permissions

**Check CSI controller logs:**
```bash
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-controller --tail=100
```

### Volume Attach Failures

**Symptom:** Pod stuck in `ContainerCreating`, events show attach errors.

**Check node plugin logs:**
```bash
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-node --tail=100
```

**Common causes:**
- VM disk limit reached (Proxmox limits disks per VM)
- Storage backend unavailable
- SCSI controller missing on VM

### Permission Denied Errors

**Symptom:** Logs show `permission denied` or `403` errors.

**Verify permissions:**
```bash
# On Proxmox host
pveum user permissions kubernetes-csi@pve
```

**Required output:**
```
/: CSI (VM.Audit, VM.Config.Disk, Datastore.Allocate, Datastore.AllocateSpace, Datastore.Audit)
```

### API Connection Issues

**Test connectivity from cluster:**
```bash
kubectl run -it --rm curl --image=curlimages/curl --restart=Never -- \
  curl -k https://<proxmox-host>:8006/api2/json/version
```

**Common causes:**
- Firewall blocking port 8006
- Incorrect endpoint URL
- DNS resolution failure

## Limitations

**Proxmox CSI Driver v0.13.0 limitations:**

| Feature | Support |
|---------|---------|
| Dynamic provisioning | ✅ Yes |
| Volume attachment | ✅ Yes |
| Volume expansion | ❌ No |
| Snapshots | ⚠️ Depends on storage backend |
| Cloning | ❌ No |
| ReadWriteMany | ❌ No (RWO only) |

**Performance considerations:**
- Network latency between K8s nodes and Proxmox API
- Storage backend type significantly affects I/O performance
- LVM is faster than Ceph for single-node clusters

**Backup recommendations:**
- Use Velero or similar for PV backups
- Consider Proxmox-level snapshots for disaster recovery
- Test restore procedures regularly

## Files in This Directory

| File | Description |
|------|-------------|
| `README.md` | This documentation file |
| `helm-values.yaml` | Helm chart values for Proxmox CSI v0.13.0 |
| `proxmox-credentials-secret.yaml` | Template for Proxmox credentials Secret |
| `storageclass.yaml` | Standalone StorageClass manifest (mutually exclusive with Helm-managed) |
| `kustomization.yaml` | Kustomization for Flux GitOps (Phase 8) |
| `.gitignore` | Prevents committing sensitive credential files |
| `verification/namespace.yaml` | Test namespace (apply first before other test manifests) |
| `verification/test-pvc.yaml` | Test manifest for PVC provisioning |
| `verification/test-statefulset.yaml` | Test manifest for StatefulSet volumes |

## References

- [Proxmox CSI Plugin GitHub](https://github.com/sergelogvinov/proxmox-csi-plugin)
- [Proxmox CSI Plugin Helm Chart](https://github.com/sergelogvinov/helm-charts/tree/master/charts/proxmox-csi-plugin)
- [Proxmox API Documentation](https://pve.proxmox.com/pve-docs/api-viewer/)
- [Kubernetes CSI Documentation](https://kubernetes-csi.github.io/docs/)
- [Talos Storage Documentation](https://www.talos.dev/v1.11/kubernetes-guides/configuration/storage/)
- [Proxmox Storage Configuration](https://pve.proxmox.com/pve-docs/chapter-pvesm.html)
