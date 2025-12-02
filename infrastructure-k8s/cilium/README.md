# Cilium CNI for Talos Kubernetes

This directory contains configuration for deploying Cilium as the Container Network Interface (CNI) for the Talos Kubernetes cluster with Gateway API support and Hubble observability.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [Manual Installation (Phase 6)](#manual-installation-phase-6)
- [Verification](#verification)
- [Troubleshooting](#troubleshooting)
- [Gateway API Usage](#gateway-api-usage)
- [Hubble Observability](#hubble-observability)
- [References](#references)

## Architecture Overview

**Cilium Components:**
- **Cilium Agent (DaemonSet):** Runs on every node, manages eBPF programs and networking
- **Cilium Operator:** Manages cluster-wide resources and IP allocation
- **Hubble Relay:** Aggregates observability data from all nodes
- **Hubble UI:** Web interface for network visualization

**Key Features:**
- **eBPF Dataplane:** High-performance packet processing without iptables
- **Kube-proxy Replacement:** Native service load balancing via eBPF
- **Gateway API:** Modern ingress controller using Kubernetes Gateway API
- **Hubble:** Deep network observability and flow visibility

**Talos Integration:**
```
┌─────────────────────────────────────────────────────────────────┐
│                     Talos Kubernetes Cluster                     │
├─────────────────────────────────────────────────────────────────┤
│  KubePrism (localhost:7445)  ←── Cilium API Connection          │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ Control Plane │  │ Control Plane │  │ Control Plane │          │
│  │   Node 1      │  │   Node 2      │  │   Node 3      │          │
│  │ ┌──────────┐ │  │ ┌──────────┐ │  │ ┌──────────┐ │          │
│  │ │ Cilium   │ │  │ │ Cilium   │ │  │ │ Cilium   │ │          │
│  │ │ Agent    │ │  │ │ Agent    │ │  │ │ Agent    │ │          │
│  │ └──────────┘ │  │ └──────────┘ │  │ └──────────┘ │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
│              ↓              ↓              ↓                    │
│           eBPF Dataplane (pod-to-pod via native routing)        │
│                 Pod Network: 10.244.0.0/16                      │
└─────────────────────────────────────────────────────────────────┘
```

## Prerequisites

Before installing Cilium, ensure:

1. **Talos cluster bootstrapped** (Phase 4 complete)
   - Nodes should be in `NotReady` state (waiting for CNI)
   - Verify: `kubectl get nodes`

2. **CNI set to 'none' in Talos config**
   - Confirmed in `infrastructure/lib/constructs/talos-cluster-bootstrap.ts`
   - Talos is configured to not install a default CNI

3. **kubectl access configured**
   ```bash
   cd infrastructure
   cdktf output -raw kubeconfig_raw | base64 -d > ~/.kube/config
   kubectl get nodes
   ```

4. **Helm 3.x installed**
   ```bash
   helm version
   # Should show v3.x.x
   ```

## Manual Installation (Phase 6)

### Step 1: Install Gateway API CRDs

Gateway API CRDs must be installed **before** Cilium deployment:

```bash
kubectl apply --server-side -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.4.0/experimental-install.yaml
```

Verify CRDs are installed:

```bash
kubectl get crd gateways.gateway.networking.k8s.io
kubectl get crd httproutes.gateway.networking.k8s.io
kubectl get crd gatewayclasses.gateway.networking.k8s.io
```

### Step 2: Add Cilium Helm Repository

```bash
helm repo add cilium https://helm.cilium.io/
helm repo update
```

### Step 3: Install Cilium

```bash
cd infrastructure-k8s/cilium

helm install cilium cilium/cilium \
  --version 1.18.4 \
  --namespace kube-system \
  --values helm-values.yaml
```

### Step 4: Wait for Cilium to be Ready

```bash
kubectl -n kube-system rollout status daemonset/cilium --timeout=5m
kubectl -n kube-system rollout status deployment/cilium-operator --timeout=5m
```

### Step 5: Verify Node Status

Nodes should transition to `Ready` state:

```bash
kubectl get nodes
# All nodes should show STATUS: Ready
```

## Verification

### Basic Checks

```bash
# Check Cilium pods are running
kubectl get pods -n kube-system -l k8s-app=cilium

# Check Cilium operator
kubectl get pods -n kube-system -l name=cilium-operator

# Check Hubble components
kubectl get pods -n kube-system -l k8s-app=hubble-relay
kubectl get pods -n kube-system -l k8s-app=hubble-ui
```

### Cilium CLI (Optional)

Install the Cilium CLI for advanced diagnostics:

```bash
# macOS
brew install cilium-cli

# Linux
CILIUM_CLI_VERSION=$(curl -s https://raw.githubusercontent.com/cilium/cilium-cli/main/stable.txt)
curl -L --fail --remote-name-all https://github.com/cilium/cilium-cli/releases/download/${CILIUM_CLI_VERSION}/cilium-linux-amd64.tar.gz
sudo tar xzvfC cilium-linux-amd64.tar.gz /usr/local/bin
```

Use Cilium CLI:

```bash
# Check Cilium status
cilium status --wait

# Run connectivity test (comprehensive)
cilium connectivity test
```

### Test Pod Connectivity

Apply the test manifests from the verification directory:

```bash
# Create test pods
kubectl apply -f verification/test-connectivity.yaml

# Wait for pods to be ready
kubectl wait --for=condition=Ready pod -l app=test-server -n cilium-test --timeout=60s
kubectl wait --for=condition=Ready pod -l app=test-client -n cilium-test --timeout=60s

# Test connectivity
kubectl exec -n cilium-test test-client -- wget -qO- http://test-server:80

# Cleanup
kubectl delete -f verification/test-connectivity.yaml
```

### Verify Gateway API

```bash
# Check GatewayClass
kubectl get gatewayclass
# Should show 'cilium' GatewayClass

# Test Gateway API (optional)
kubectl apply -f verification/test-gateway-api.yaml
kubectl get gateway -n cilium-test
kubectl get httproute -n cilium-test

# Cleanup
kubectl delete -f verification/test-gateway-api.yaml
```

## Troubleshooting

### Pods Stuck in Pending/ContainerCreating

**Symptom:** Pods remain in Pending or ContainerCreating state.

**Check Cilium agent logs:**
```bash
kubectl logs -n kube-system -l k8s-app=cilium --tail=100
```

**Common causes:**
- Cilium agent not running - check DaemonSet status
- IPAM exhaustion - check IP allocation
- Security context issues - verify capabilities in helm-values.yaml

### Network Connectivity Issues

**Symptom:** Pods cannot communicate with each other or external services.

**Diagnostic steps:**
```bash
# Check Cilium status
cilium status

# Check endpoint connectivity
kubectl exec -n kube-system -l k8s-app=cilium -- cilium endpoint list

# Check BPF maps
kubectl exec -n kube-system -l k8s-app=cilium -- cilium bpf lb list
```

**Common causes:**
- KubePrism endpoint not reachable - verify `k8sServiceHost: localhost` and `k8sServicePort: 7445`
- Native routing misconfigured - check `ipv4NativeRoutingCIDR` matches pod CIDR
- Tunnel mode enabled - should be `tunnel: disabled` for native routing

### Cilium Agent CrashLoopBackOff

**Symptom:** Cilium agent pods crash repeatedly.

**Check logs:**
```bash
kubectl logs -n kube-system -l k8s-app=cilium --previous
```

**Common causes:**
- Missing security capabilities - ensure all capabilities in helm-values.yaml
- BPF filesystem not mounted - check `/sys/fs/bpf` mount
- Kernel incompatibility - Talos v1.11.5 should be compatible

### Gateway API CRDs Missing

**Symptom:** Cilium Gateway API controller fails to start or Gateway resources rejected.

**Verify CRDs:**
```bash
kubectl get crd | grep gateway
```

**Solution:** Install CRDs before Cilium:
```bash
kubectl apply --server-side -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.4.0/experimental-install.yaml
```

### Permission Denied Errors

**Symptom:** Cilium logs show permission denied or capability errors.

**Solution:** Verify security context in helm-values.yaml includes all required capabilities:
```yaml
securityContext:
  capabilities:
    ciliumAgent:
      - CHOWN
      - KILL
      - NET_ADMIN
      - NET_RAW
      - IPC_LOCK
      - SYS_ADMIN
      - SYS_RESOURCE
      - DAC_OVERRIDE
      - FOWNER
      - SETGID
      - SETUID
    cleanCiliumState:
      - NET_ADMIN
      - SYS_ADMIN
      - SYS_RESOURCE
```

## Gateway API Usage

Cilium implements the Kubernetes Gateway API for ingress traffic management.

### Check GatewayClass

```bash
kubectl get gatewayclass
```

The `cilium` GatewayClass should be available after Cilium installation.

### Create a Gateway

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: my-gateway
  namespace: default
spec:
  gatewayClassName: cilium
  listeners:
    - name: http
      protocol: HTTP
      port: 80
```

### Create HTTPRoute

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: my-route
  namespace: default
spec:
  parentRefs:
    - name: my-gateway
  rules:
    - matches:
        - path:
            type: PathPrefix
            value: /
      backendRefs:
        - name: my-service
          port: 80
```

**Note:** Full Gateway configuration with TLS and cert-manager integration will be covered in Phase 9.

## Hubble Observability

Hubble provides network visibility for debugging and monitoring.

### Access Hubble UI

```bash
kubectl port-forward -n kube-system svc/hubble-ui 12000:80
# Open http://localhost:12000 in browser
```

### Hubble CLI

Install Hubble CLI:

```bash
# macOS
brew install hubble

# Linux
HUBBLE_VERSION=$(curl -s https://raw.githubusercontent.com/cilium/hubble/master/stable.txt)
curl -L --fail --remote-name-all https://github.com/cilium/hubble/releases/download/${HUBBLE_VERSION}/hubble-linux-amd64.tar.gz
sudo tar xzvfC hubble-linux-amd64.tar.gz /usr/local/bin
```

Use Hubble CLI:

```bash
# Port-forward to Hubble Relay
kubectl port-forward -n kube-system svc/hubble-relay 4245:80 &

# Observe flows
hubble observe --follow

# Filter by namespace
hubble observe --namespace default

# Filter by pod
hubble observe --pod my-pod
```

### Hubble Metrics

Hubble exports Prometheus metrics for monitoring:

```bash
# Check available metrics
kubectl exec -n kube-system -l k8s-app=cilium -- curl -s localhost:9962/metrics | head -50
```

Integrate with Prometheus/Grafana in Phase 12 (Observability).

## Files in This Directory

| File | Description |
|------|-------------|
| `README.md` | This documentation file |
| `helm-values.yaml` | Helm chart values for Cilium v1.18.4 |
| `gateway-api-crds.yaml` | Gateway API CRDs installation reference |
| `kustomization.yaml` | Kustomization for Flux GitOps (Phase 7) |
| `verification/test-connectivity.yaml` | Test manifest for pod networking |
| `verification/test-gateway-api.yaml` | Test manifest for Gateway API |

## References

- [Cilium Documentation](https://docs.cilium.io/)
- [Cilium on Talos Linux](https://www.talos.dev/v1.11/kubernetes-guides/network/cilium/)
- [Gateway API Documentation](https://gateway-api.sigs.k8s.io/)
- [Hubble Documentation](https://docs.cilium.io/en/stable/gettingstarted/hubble/)
- [Cilium Helm Chart Values](https://github.com/cilium/cilium/tree/main/install/kubernetes/cilium)
- [Kubernetes Gateway API v1.4](https://github.com/kubernetes-sigs/gateway-api/releases/tag/v1.4.0)
