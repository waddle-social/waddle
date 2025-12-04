# Node Down Runbook

## Alert

**Alert**: NodeDown, NodeNotReady, NodeMemoryPressure, NodeDiskPressure
**Severity**: Critical (NodeDown, NodeNotReady), Warning (pressure alerts)
**Impact**: Reduced cluster capacity, potential pod evictions, service degradation

## Overview

A node has become unreachable or is reporting unhealthy conditions. This affects workloads scheduled on that node and may trigger pod evictions.

## Diagnosis

### 1. Check Node Status

```bash
# List all nodes with status
kubectl get nodes -o wide

# Get detailed node status
kubectl describe node <node-name>

# Check node conditions
kubectl get node <node-name> -o jsonpath='{.status.conditions}' | jq
```

### 2. Check Proxmox VM Status

1. Log into Proxmox web UI: `https://<proxmox-ip>:8006`
2. Navigate to the VM corresponding to the node
3. Check VM status (running, stopped, paused)
4. Review VM resource usage (CPU, memory, disk I/O)

### 3. Check Talos Node Logs

```bash
# Get Talos logs (requires talosctl configured)
talosctl -n <node-ip> logs

# Check specific service
talosctl -n <node-ip> service kubelet

# Check etcd health (for control plane nodes)
talosctl -n <node-ip> etcd members
```

### 4. Check Network Connectivity

```bash
# Basic connectivity
ping <node-ip>

# Check if Kubernetes API is reachable from the node
talosctl -n <node-ip> health

# Check Cilium agent on the node (if accessible)
kubectl exec -n kube-system -it $(kubectl get pods -n kube-system -l k8s-app=cilium -o name | grep <node-name>) -- cilium status
```

### 5. Check Workload Impact

```bash
# List pods on the affected node
kubectl get pods -A -o wide --field-selector spec.nodeName=<node-name>

# Check for evicted pods
kubectl get pods -A | grep Evicted

# Check pending pods (may be waiting for node)
kubectl get pods -A | grep Pending
```

## Common Causes

1. **VM Power State**: VM powered off or paused in Proxmox
2. **Network Issue**: Network bridge misconfiguration, firewall rules
3. **Resource Exhaustion**: Out of memory, disk full
4. **Talos Issue**: kubelet crash, etcd failure (control plane)
5. **Hardware Failure**: Storage failure, NIC failure

## Remediation

### Scenario 1: VM Powered Off

1. In Proxmox web UI, start the VM
2. Wait for Talos to boot and node to rejoin cluster
3. Verify with `kubectl get nodes`

### Scenario 2: Network Issue

1. Check Proxmox network bridge configuration
2. Verify VM network interface assignment
3. Check firewall rules (if any)
4. Restart VM networking if needed

```bash
# From Proxmox host
qm set <vm-id> -net0 virtio,bridge=vmbr0
```

### Scenario 3: Resource Exhaustion

**Memory Pressure:**
```bash
# Check memory on node
talosctl -n <node-ip> memory

# Identify memory-heavy pods
kubectl top pods -A --sort-by=memory | head -20

# Consider evicting large pods or adding resources to VM
```

**Disk Pressure:**
```bash
# Check disk usage
talosctl -n <node-ip> disk

# Clean up unused images
talosctl -n <node-ip> image prune

# Expand VM disk in Proxmox if needed
```

### Scenario 4: Talos/Kubernetes Issue

```bash
# Reboot the node
talosctl -n <node-ip> reboot

# If kubelet is stuck
talosctl -n <node-ip> service kubelet restart

# For control plane etcd issues
talosctl -n <node-ip> etcd leave  # Only if node is completely unrecoverable
```

### Scenario 5: Persistent Failure (Node Rebuild)

If the node is unrecoverable:

1. **Cordon the node** (if still visible):
   ```bash
   kubectl cordon <node-name>
   ```

2. **Drain workloads** (if possible):
   ```bash
   kubectl drain <node-name> --ignore-daemonsets --delete-emptydir-data
   ```

3. **Remove from cluster**:
   ```bash
   kubectl delete node <node-name>
   ```

4. **Rebuild VM** via CDKTF:
   ```bash
   cd infrastructure
   bun run deploy
   ```

5. **Re-provision with Talos**:
   Follow Talos setup documentation.

## Verification

After remediation, verify:

```bash
# Node is ready
kubectl get nodes

# All pods running
kubectl get pods -A -o wide --field-selector spec.nodeName=<node-name>

# No alerts firing
kubectl get prometheusrules -n observability
```

## Escalation

- **15 minutes**: If basic remediation fails, escalate to Platform Team
- **30 minutes**: If node is unrecoverable, begin rebuild procedure
- **1 hour**: Escalate to Infrastructure Lead for major outages

## Related Documentation

- [Teleport Setup](../teleport-setup.md) - Secure access to cluster
- [Talos Documentation](https://www.talos.dev/docs/)
- [Proxmox Documentation](https://pve.proxmox.com/wiki/Main_Page)
