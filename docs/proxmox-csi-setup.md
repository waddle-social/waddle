# Proxmox CSI Driver Setup and Operations Guide

This guide covers the complete setup and operation of the Proxmox CSI driver for persistent storage in the Talos Kubernetes cluster.

## Table of Contents

1. [Introduction](#introduction)
2. [Prerequisites](#prerequisites)
3. [Proxmox User and Permissions Setup](#proxmox-user-and-permissions-setup)
4. [Kubernetes Secret Creation](#kubernetes-secret-creation)
5. [CSI Driver Installation](#csi-driver-installation)
6. [StorageClass Configuration](#storageclass-configuration)
7. [Testing and Verification](#testing-and-verification)
8. [Troubleshooting](#troubleshooting)
9. [Operations](#operations)
10. [Limitations and Considerations](#limitations-and-considerations)
11. [Integration with Other Components](#integration-with-other-components)
12. [References](#references)

## Introduction

### Overview

The Proxmox CSI driver enables Kubernetes workloads to use persistent storage from Proxmox storage backends. It implements the Container Storage Interface (CSI) specification to provide dynamic volume provisioning, attachment, and lifecycle management.

### Benefits

- **Dynamic Provisioning:** Automatically create storage volumes when PVCs are created
- **Kubernetes-Native:** Standard StorageClass and PVC patterns
- **Multiple Storage Backends:** Support for LVM, ZFS, Ceph, NFS, and other Proxmox storage types
- **Topology Awareness:** Zone-aware volume placement for better scheduling

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Kubernetes Cluster                                 │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         csi-proxmox namespace                          │ │
│  │                                                                         │ │
│  │   ┌─────────────────────┐          ┌─────────────────────┐            │ │
│  │   │   CSI Controller    │          │   CSI Node Plugin   │            │ │
│  │   │    (Deployment)     │          │    (DaemonSet)      │            │ │
│  │   │                     │          │                     │            │ │
│  │   │ - CreateVolume      │          │ - NodeStageVolume   │            │ │
│  │   │ - DeleteVolume      │          │ - NodePublishVolume │            │ │
│  │   │ - ControllerPublish │          │ - Mount/Unmount     │            │ │
│  │   └──────────┬──────────┘          └──────────┬──────────┘            │ │
│  │              │                                │                        │ │
│  └──────────────┼────────────────────────────────┼────────────────────────┘ │
│                 │                                │                          │
│                 ▼                                ▼                          │
│         ┌──────────────┐                ┌──────────────┐                   │
│         │ Proxmox API  │                │  VM Disks    │                   │
│         │  (port 8006) │                │  Attachment  │                   │
│         └──────┬───────┘                └──────────────┘                   │
│                │                                                            │
└────────────────┼────────────────────────────────────────────────────────────┘
                 │
                 ▼
┌────────────────────────────────────────────────────────────────────────────┐
│                          Proxmox VE Host                                    │
│                                                                             │
│   ┌─────────────────────────────────────────────────────────────────────┐  │
│   │                    Storage Backends                                  │  │
│   │                                                                      │  │
│   │   ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌────────────┐   │  │
│   │   │ local-lvm  │  │ local-zfs  │  │ ceph-pool  │  │    nfs     │   │  │
│   │   │   (LVM)    │  │   (ZFS)    │  │   (Ceph)   │  │   (NFS)    │   │  │
│   │   └────────────┘  └────────────┘  └────────────┘  └────────────┘   │  │
│   └─────────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────────────┘
```

## Prerequisites

Before installing the Proxmox CSI driver, ensure you have:

### Infrastructure Requirements

- **Proxmox VE 9.1** with API access
- **Talos Kubernetes cluster** with Cilium CNI (Phase 6 complete)
- **Proxmox storage backend** configured (LVM, ZFS, Ceph, etc.)

### Tools Required

- **kubectl** configured with cluster access
- **Helm 3.x** for chart installation
- **SSH access** to Proxmox host (for user/token setup)

### Verification Commands

```bash
# Verify cluster access
kubectl get nodes
# All nodes should show STATUS: Ready

# Verify Cilium is running
kubectl get pods -n kube-system -l k8s-app=cilium
# All pods should be Running

# Verify Proxmox API (from workstation with network access)
curl -k https://<proxmox-host>:8006/api2/json/version

# Verify Proxmox storage (SSH to Proxmox host)
pvesm status
# Lists available storage backends with status
```

## Proxmox User and Permissions Setup

### Why a Dedicated CSI User?

Using a dedicated user for the CSI driver provides:
- **Security:** Minimal permissions (principle of least privilege)
- **Auditability:** Track CSI operations separately in Proxmox logs
- **Revocability:** Easy to rotate or revoke access without affecting other systems

### Required Permissions

| Permission | Purpose |
|------------|---------|
| `VM.Audit` | Read VM configuration to verify disk attachment |
| `VM.Config.Disk` | Attach and detach disks to/from VMs |
| `Datastore.Allocate` | Create new volumes on storage backends |
| `Datastore.AllocateSpace` | Reserve disk space for volumes |
| `Datastore.Audit` | Read storage status and available space |

### CLI Setup (Recommended)

SSH to your Proxmox host:

```bash
ssh root@<proxmox-host>
```

Create the CSI role and user:

```bash
# Step 1: Create CSI role with required permissions
pveum role add CSI -privs "VM.Audit VM.Config.Disk Datastore.Allocate Datastore.AllocateSpace Datastore.Audit"

# Step 2: Create dedicated user for CSI driver
pveum user add kubernetes-csi@pve

# Step 3: Assign role to user at root path (all resources)
pveum aclmod / -user kubernetes-csi@pve -role CSI

# Step 4: Create API token WITHOUT privilege separation
# IMPORTANT: -privsep 0 ensures token inherits user's permissions
pveum user token add kubernetes-csi@pve csi -privsep 0
```

**Save the token output immediately!** It's displayed only once:

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

### Web UI Setup (Alternative)

1. **Create Role:**
   - Navigate to: Datacenter → Permissions → Roles → Create
   - Name: `CSI`
   - Privileges: Select `VM.Audit`, `VM.Config.Disk`, `Datastore.Allocate`, `Datastore.AllocateSpace`, `Datastore.Audit`
   - Click Create

2. **Create User:**
   - Navigate to: Datacenter → Permissions → Users → Add
   - User name: `kubernetes-csi`
   - Realm: `pve` (Proxmox VE authentication)
   - Leave password empty (token-only access)
   - Click Add

3. **Create API Token:**
   - Navigate to: Datacenter → Permissions → API Tokens → Add
   - User: `kubernetes-csi@pve`
   - Token ID: `csi`
   - **Important:** Uncheck "Privilege Separation"
   - Click Add
   - **Copy and save the token value** (displayed only once!)

4. **Assign Role:**
   - Navigate to: Datacenter → Permissions → Add → User Permission
   - Path: `/` (root - all resources)
   - User: `kubernetes-csi@pve`
   - Role: `CSI`
   - Click Add

### Verify Setup

```bash
# List users
pveum user list | grep kubernetes-csi

# List ACLs
pveum acl list | grep kubernetes-csi

# List tokens
pveum user token list kubernetes-csi@pve

# Test API token connectivity
curl -k -H "Authorization: PVEAPIToken=kubernetes-csi@pve!csi=<your-token-secret>" \
  https://<proxmox-host>:8006/api2/json/version
```

## Kubernetes Secret Creation

### Deriving Values from Environment Variables

The CSI configuration should be consistent with VM provisioning settings. The `infrastructure/.env.example` file defines the canonical source for CSI-related values:

| Environment Variable | CSI Config Field | Example Value |
|---------------------|------------------|---------------|
| `PROXMOX_CSI_ENDPOINT` | `url` (append `/api2/json`) | `https://proxmox.waddle.social:8006/api2/json` |
| `PROXMOX_CSI_TOKEN_ID` | `token_id` | `kubernetes-csi@pve!csi` |
| `PROXMOX_CSI_INSECURE` | `insecure` | `false` |
| `PROXMOX_CSI_REGION` | `region` | `proxmox` |
| `PROXMOX_CSI_STORAGE_ID` | `storageClass[].storage` in Helm values | `local-lvm` |

**Note:** The `token_secret` is NOT stored in `.env` for security reasons - it should only exist temporarily during secret creation.

### Create the Configuration File

Create a local file with your Proxmox credentials (this file should never be committed to Git):

```bash
# Derive values from your .env configuration
cat > proxmox-csi-config.yaml <<EOF
clusters:
  - url: https://proxmox.waddle.social:8006/api2/json  # PROXMOX_CSI_ENDPOINT + /api2/json
    insecure: false                                   # PROXMOX_CSI_INSECURE
    token_id: "kubernetes-csi@pve!csi"               # PROXMOX_CSI_TOKEN_ID
    token_secret: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
    region: proxmox                                   # PROXMOX_CSI_REGION
EOF
```

Replace:
- `proxmox.waddle.social` with your Proxmox hostname or IP (from `PROXMOX_CSI_ENDPOINT`)
- `token_secret` with the UUID from the token creation step
- `region` with your topology region (should match `TALOS_TOPOLOGY_REGION` and `PROXMOX_CSI_REGION`)

**Note:** Set `insecure: true` only for self-signed certificates (not recommended for production).

### Create the Kubernetes Secret

```bash
# Create namespace
kubectl create namespace csi-proxmox

# Create secret from file
kubectl create secret generic proxmox-csi-credentials \
  --from-file=config.yaml=proxmox-csi-config.yaml \
  -n csi-proxmox

# Verify secret was created
kubectl get secret proxmox-csi-credentials -n csi-proxmox
```

### Security: Delete Local Credentials File

```bash
rm proxmox-csi-config.yaml
```

### Verify Secret Contents

```bash
# Decode and view secret (for verification only)
kubectl get secret proxmox-csi-credentials -n csi-proxmox \
  -o jsonpath='{.data.config\.yaml}' | base64 -d
```

## CSI Driver Installation

### Add Helm Repository

```bash
helm repo add proxmox-csi https://sergelogvinov.github.io/proxmox-csi-plugin
helm repo update
```

### Install CSI Driver

```bash
cd infrastructure-k8s/storage

helm install proxmox-csi proxmox-csi/proxmox-csi-plugin \
  --version 0.13.0 \
  --namespace csi-proxmox \
  --values helm-values.yaml
```

### Verify Installation

```bash
# Check CSI controller pod
kubectl get pods -n csi-proxmox -l app=proxmox-csi-plugin-controller
# Expected: 1/1 Running

# Check CSI node pods (one per cluster node)
kubectl get pods -n csi-proxmox -l app=proxmox-csi-plugin-node
# Expected: One Running pod per node

# Check CSI driver registered
kubectl get csidrivers
# Should show: csi.proxmox.sinextra.dev

# Check StorageClass created
kubectl get storageclass proxmox-csi
# Should show: proxmox-csi (default)
```

### Check Logs

```bash
# Controller logs
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-controller --tail=50

# Node plugin logs (on a specific node)
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-node --tail=50
```

## StorageClass Configuration

### Default StorageClass

The Helm chart creates a default StorageClass with these settings:

| Parameter | Value | Description |
|-----------|-------|-------------|
| `name` | `proxmox-csi` | StorageClass name |
| `storage` | `local-lvm` | Proxmox storage backend ID |
| `cache` | `writethrough` | Disk cache mode |
| `reclaimPolicy` | `Delete` | Delete volume when PVC deleted |
| `volumeBindingMode` | `WaitForFirstConsumer` | Bind when pod scheduled |

### Finding Your Storage ID

On Proxmox host:
```bash
# List all storage
pvesm status

# Example output:
# Name         Type     Status           Total            Used       Available
# local        dir      active        96636048         5664348        91971700
# local-lvm    lvmthin  active       102359040        10240000        92119040
# ceph-pool    rbd      active      1073741824       107374182       966367642
```

Use the `Name` column value for your storage ID (e.g., `local-lvm`).

### Cache Modes

| Mode | Safety | Performance | Use Case |
|------|--------|-------------|----------|
| `directsync` | ★★★★★ | ★★☆☆☆ | Databases, critical data |
| `writethrough` | ★★★★☆ | ★★★☆☆ | General workloads (default) |
| `writeback` | ★★☆☆☆ | ★★★★★ | Temporary data, caches |
| `none` | ★★★☆☆ | ★★★☆☆ | Let storage decide |

### Creating Additional StorageClasses

For different storage tiers or backends:

```yaml
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: proxmox-csi-database
provisioner: csi.proxmox.sinextra.dev
parameters:
  storage: local-zfs      # ZFS for better data integrity
  cache: directsync       # Maximum data safety
reclaimPolicy: Retain     # Keep volumes for recovery
volumeBindingMode: WaitForFirstConsumer
```

Apply with: `kubectl apply -f <filename>.yaml`

## Testing and Verification

### Quick PVC Test

```bash
# Apply namespace first (required)
kubectl apply -f infrastructure-k8s/storage/verification/namespace.yaml

# Apply test manifest
kubectl apply -f infrastructure-k8s/storage/verification/test-pvc.yaml

# Watch PVC status
kubectl get pvc -n csi-test -w
# Wait for STATUS: Bound

# Check PV created
kubectl get pv | grep csi-test

# Check pod status
kubectl get pod -n csi-test
# Wait for STATUS: Running

# Verify data written
kubectl exec -n csi-test test-pod -- cat /data/test.txt
# Output: CSI test successful

# Cleanup
kubectl delete -f infrastructure-k8s/storage/verification/test-pvc.yaml
```

### StatefulSet Test

```bash
# Apply namespace first (if not already created)
kubectl apply -f infrastructure-k8s/storage/verification/namespace.yaml

# Apply StatefulSet test
kubectl apply -f infrastructure-k8s/storage/verification/test-statefulset.yaml

# Watch PVCs (should see 2)
kubectl get pvc -n csi-test -w

# Check pods
kubectl get pods -n csi-test -l app=test-statefulset

# Test data persistence
kubectl exec -n csi-test test-statefulset-0 -- cat /data/hostname.txt
kubectl delete pod test-statefulset-0 -n csi-test
kubectl wait --for=condition=Ready pod/test-statefulset-0 -n csi-test --timeout=120s
kubectl exec -n csi-test test-statefulset-0 -- cat /data/hostname.txt
# Should still show: test-statefulset-0

# Cleanup
kubectl delete -f infrastructure-k8s/storage/verification/test-statefulset.yaml
kubectl delete pvc -n csi-test -l app=test-statefulset
```

### Verify in Proxmox UI

1. Open Proxmox web interface
2. Navigate to the VM running your test pod
3. Click on Hardware
4. Look for new disk attachment (e.g., `scsi1: local-lvm:vm-XXX-disk-X`)

## Troubleshooting

### CSI Controller Pod CrashLoopBackOff

**Symptoms:** Controller pod repeatedly crashes.

**Check logs:**
```bash
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-controller --previous
```

**Common causes and solutions:**

| Cause | Solution |
|-------|----------|
| Invalid credentials | Verify Secret contents match Proxmox token |
| API unreachable | Check network connectivity to Proxmox:8006 |
| SSL certificate error | Set `insecure: true` for self-signed certs |
| Wrong API URL format | Use `https://host:8006/api2/json` format |

### PVC Stuck in Pending

**Symptoms:** PVC remains in Pending state.

**Check events:**
```bash
kubectl describe pvc <pvc-name> -n <namespace>
```

**Common causes:**

| Cause | Solution |
|-------|----------|
| StorageClass not found | Verify `storageClassName` matches |
| Storage backend full | Check Proxmox storage capacity |
| Permission denied | Verify CSI user has required permissions |
| Controller not running | Check controller pod status |

### Volume Attach Failures

**Symptoms:** Pod stuck in ContainerCreating, events show attach errors.

**Check node plugin logs:**
```bash
kubectl logs -n csi-proxmox -l app=proxmox-csi-plugin-node
```

**Common causes:**
- VM disk limit reached (Proxmox limits ~256 disks per VM)
- Storage backend unavailable
- SCSI controller not present on VM

### Permission Denied Errors

**Verify permissions on Proxmox:**
```bash
pveum user permissions kubernetes-csi@pve
```

**Expected output should include:**
```
/: CSI (VM.Audit, VM.Config.Disk, Datastore.Allocate, Datastore.AllocateSpace, Datastore.Audit)
```

### API Connection Issues

**Test from cluster:**
```bash
kubectl run -it --rm curl-test --image=curlimages/curl --restart=Never -- \
  curl -k https://<proxmox-host>:8006/api2/json/version
```

## Operations

### Volume Lifecycle Management

**List all CSI volumes:**
```bash
kubectl get pv -o custom-columns=NAME:.metadata.name,STORAGE:.spec.csi.volumeHandle,STATUS:.status.phase
```

**Force delete stuck PV:**
```bash
kubectl patch pv <pv-name> -p '{"metadata":{"finalizers":null}}'
kubectl delete pv <pv-name>
```

### Rotating API Tokens

1. **Create new token:**
   ```bash
   pveum user token add kubernetes-csi@pve csi-new -privsep 0
   ```

2. **Update Kubernetes Secret:**
   ```bash
   # Create new config file with new token
   cat > proxmox-csi-config-new.yaml <<EOF
   clusters:
     - url: https://proxmox.waddle.social:8006/api2/json
       insecure: false
       token_id: "kubernetes-csi@pve!csi-new"
       token_secret: "<new-token-secret>"
       region: proxmox
   EOF

   # Update secret
   kubectl create secret generic proxmox-csi-credentials \
     --from-file=config.yaml=proxmox-csi-config-new.yaml \
     -n csi-proxmox \
     --dry-run=client -o yaml | kubectl apply -f -

   # Delete local file
   rm proxmox-csi-config-new.yaml
   ```

3. **Restart CSI controller:**
   ```bash
   kubectl rollout restart deployment -n csi-proxmox -l app=proxmox-csi-plugin-controller
   ```

4. **Delete old token:**
   ```bash
   pveum user token remove kubernetes-csi@pve csi
   ```

### Upgrading the CSI Driver

```bash
helm repo update
helm upgrade proxmox-csi proxmox-csi/proxmox-csi-plugin \
  --version <new-version> \
  --namespace csi-proxmox \
  --values infrastructure-k8s/storage/helm-values.yaml
```

### Monitoring CSI Health

```bash
# Check controller status
kubectl get deployment -n csi-proxmox

# Check node plugin status
kubectl get daemonset -n csi-proxmox

# Check recent events
kubectl get events -n csi-proxmox --sort-by='.lastTimestamp'
```

## Limitations and Considerations

### Feature Support Matrix

| Feature | Support |
|---------|---------|
| Dynamic Provisioning | ✅ Supported |
| Volume Attachment | ✅ Supported |
| Volume Expansion | ❌ Not supported (v0.13.0) |
| Snapshots | ⚠️ Storage backend dependent |
| Cloning | ❌ Not supported |
| ReadWriteMany (RWX) | ❌ Not supported (RWO only) |
| Raw Block Volumes | ❌ Not supported |

### Performance Considerations

- **Network latency:** Volume operations go through Proxmox API
- **Storage backend:** LVM > NFS for IOPS, Ceph for scalability
- **Cache mode:** Significant impact on write performance

### Backup Recommendations

- Use Velero for Kubernetes-native PV backups
- Consider Proxmox-level snapshots for disaster recovery
- Test restore procedures regularly
- Document recovery time objectives (RTO)

## Integration with Other Components

### CloudNativePG (Phase 10)

CloudNativePG will use Proxmox CSI for PostgreSQL data volumes:

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: postgres
spec:
  instances: 3
  storage:
    storageClass: proxmox-csi
    size: 10Gi
```

### Observability Stack (Phase 12)

Prometheus, Loki, and other observability components will use persistent storage:

```yaml
# Example: Prometheus storage
persistence:
  enabled: true
  storageClassName: proxmox-csi
  size: 50Gi
```

### Future: Velero Backup Integration

```yaml
# Example: Velero backup configuration
apiVersion: velero.io/v1
kind: BackupStorageLocation
spec:
  provider: csi
  config:
    storageClassName: proxmox-csi
```

## References

- [Proxmox CSI Plugin GitHub](https://github.com/sergelogvinov/proxmox-csi-plugin)
- [Proxmox CSI Plugin Helm Chart](https://github.com/sergelogvinov/helm-charts/tree/master/charts/proxmox-csi-plugin)
- [Proxmox VE API Documentation](https://pve.proxmox.com/pve-docs/api-viewer/)
- [Proxmox Storage Configuration](https://pve.proxmox.com/pve-docs/chapter-pvesm.html)
- [Kubernetes CSI Specification](https://kubernetes-csi.github.io/docs/)
- [Talos Linux Storage Guide](https://www.talos.dev/v1.11/kubernetes-guides/configuration/storage/)
- [CSI Driver Development Guide](https://kubernetes-csi.github.io/docs/introduction.html)
