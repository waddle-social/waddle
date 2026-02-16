#!/usr/bin/env bash
set -euo pipefail

CILIUM_VERSION="1.19.0"
DEMOCRATIC_CSI_VERSION="0.15.1"
FLUX_OPERATOR_VERSION="0.40.0"
GATEWAY_API_VERSION="v1.3.0"

TALOS_VIP="10.10.0.20"
CILIUM_GW_VIP="10.10.0.30"

echo "=========================================="
echo "  Kubernetes Bootstrap (Phase 5)"
echo "=========================================="

# ---- 5a. Gateway API CRDs + Cilium ----
echo ""
echo "==> [5a] Installing Gateway API CRDs ${GATEWAY_API_VERSION}..."
kubectl apply -f "https://github.com/kubernetes-sigs/gateway-api/releases/download/${GATEWAY_API_VERSION}/standard-install.yaml"
kubectl apply -f "https://github.com/kubernetes-sigs/gateway-api/releases/download/${GATEWAY_API_VERSION}/experimental-install.yaml"

echo "==> Installing Cilium ${CILIUM_VERSION}..."
helm repo add cilium https://helm.cilium.io/ 2>/dev/null || true
helm repo update cilium

helm upgrade --install cilium cilium/cilium \
  --version "${CILIUM_VERSION}" \
  --namespace kube-system \
  --set ipam.mode=kubernetes \
  --set l2announcements.enabled=true \
  --set externalIPs.enabled=true \
  --set gatewayAPI.enabled=true \
  --set gatewayAPI.hostNetwork.enabled=false \
  --set kubeProxyReplacement=true \
  --set k8sServiceHost="${TALOS_VIP}" \
  --set k8sServicePort=6443 \
  --set securityContext.capabilities.ciliumAgent="{CHOWN,KILL,NET_ADMIN,NET_RAW,IPC_LOCK,SYS_ADMIN,SYS_RESOURCE,DAC_OVERRIDE,FOWNER,SETGID,SETUID}" \
  --set securityContext.capabilities.cleanCiliumState="{NET_ADMIN,SYS_ADMIN,SYS_RESOURCE}" \
  --set cgroup.autoMount.enabled=false \
  --set cgroup.hostRoot=/sys/fs/cgroup \
  --set bpf.mount.autoMount=false \
  --set bpf.mount.path=/sys/fs/bpf \
  --wait --timeout 5m

echo "==> Applying Cilium L2 announcement policy and IP pool..."
kubectl apply -f - <<EOF
apiVersion: cilium.io/v2alpha1
kind: CiliumL2AnnouncementPolicy
metadata:
  name: default-l2-policy
spec:
  loadBalancerIPs: true
  interfaces:
    - ^eth[0-9]+
---
apiVersion: cilium.io/v2alpha1
kind: CiliumLoadBalancerIPPool
metadata:
  name: gateway-pool
spec:
  blocks:
    - cidr: ${CILIUM_GW_VIP}/32
EOF

echo "==> Waiting for nodes to become Ready..."
kubectl wait --for=condition=Ready nodes --all --timeout=300s

echo "==> Cilium installed. Nodes:"
kubectl get nodes

# ---- 5b. democratic-csi ----
echo ""
echo "==> [5b] Installing democratic-csi ${DEMOCRATIC_CSI_VERSION}..."
helm repo add democratic-csi https://democratic-csi.github.io/charts/ 2>/dev/null || true
helm repo update democratic-csi

kubectl create namespace democratic-csi --dry-run=client -o yaml | kubectl apply -f -
kubectl label namespace democratic-csi pod-security.kubernetes.io/enforce=privileged --overwrite

if [ ! -f "democratic-csi-values.yaml" ]; then
  echo "ERROR: democratic-csi-values.yaml not found in current directory."
  echo "       Create it with connection details for your Proxmox iSCSI target."
  echo "       See: https://github.com/democratic-csi/democratic-csi#zfs-generic-iscsi"
  exit 1
fi

helm upgrade --install zfs-iscsi democratic-csi/democratic-csi \
  --version "${DEMOCRATIC_CSI_VERSION}" \
  --namespace democratic-csi --create-namespace \
  --values democratic-csi-values.yaml \
  --wait --timeout 5m

echo "==> democratic-csi installed."

# ---- 5c. Flux Operator ----
echo ""
echo "==> [5c] Installing Flux Operator ${FLUX_OPERATOR_VERSION}..."
helm upgrade --install flux-operator \
  oci://ghcr.io/controlplaneio-fluxcd/charts/flux-operator \
  --version "${FLUX_OPERATOR_VERSION}" \
  --namespace flux-system --create-namespace \
  --wait --timeout 5m

echo "==> Flux Operator installed."

# ---- 5d. Bootstrap secrets ----
echo ""
echo "==> [5d] Creating bootstrap secrets..."
echo "    You will be prompted for secret file paths."

kubectl create namespace 1password --dry-run=client -o yaml | kubectl apply -f -

read -rp "Path to 1password-credentials.json: " OP_CREDS_PATH
kubectl create secret generic onepassword-credentials \
  --namespace 1password \
  --from-file=1password-credentials.json="${OP_CREDS_PATH}" \
  --dry-run=client -o yaml | kubectl apply -f -

read -rsp "1Password Connect token: " OP_TOKEN
echo
kubectl create secret generic onepassword-token \
  --namespace 1password \
  --from-literal=token="${OP_TOKEN}" \
  --dry-run=client -o yaml | kubectl apply -f -

read -rsp "GitHub PAT (fine-grained, read-only on waddle-social/waddle-infra): " GITHUB_TOKEN
echo
kubectl create secret generic flux-system \
  --namespace flux-system \
  --from-literal=username=flux \
  --from-literal=password="${GITHUB_TOKEN}" \
  --dry-run=client -o yaml | kubectl apply -f -

echo "==> Bootstrap secrets created."

# ---- 5e. FluxInstance ----
echo ""
echo "==> [5e] Applying FluxInstance..."
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
    url: "https://github.com/waddle-social/waddle-infra.git"
    ref: "refs/heads/main"
    path: "platform/clusters/scaleway"
    pullSecret: "flux-system"
EOF

echo "==> FluxInstance applied. Flux will begin reconciling from the platform repo."

echo ""
echo "=========================================="
echo "  Bootstrap complete!"
echo "=========================================="
echo ""
echo "Verify with:"
echo "  kubectl get pods -A"
echo "  kubectl get fluxinstance -n flux-system"
echo ""
echo "Next: Populate waddle-social/platform repo with Flux kustomizations (Phase 6)."
