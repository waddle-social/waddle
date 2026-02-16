#!/usr/bin/env bash
set -euo pipefail

# Phase 7: Configure Teleport Kubernetes Access
#
# Run this ON the Teleport VM (deploy@teleport).
#
# Prerequisites:
#   - Teleport is running
#   - Talos cluster is bootstrapped
#   - kubeconfig exists (generated during Phase 4 bootstrap)
#   - Port 3033 forwarded on Proxmox host (iptables DNAT to 10.10.0.2:3033)

TALOS_VIP="10.10.0.20"
KUBE_CLUSTER_NAME="waddle"
KUBECONFIG_SRC="${1:-/tmp/kubeconfig}"

echo "=========================================="
echo "  Phase 7: Teleport K8s Access Setup"
echo "=========================================="

# ---- Step 1: Validate kubeconfig exists ----
if [ ! -f "${KUBECONFIG_SRC}" ]; then
  echo "ERROR: kubeconfig not found at ${KUBECONFIG_SRC}"
  echo "       Generate it with: talosctl kubeconfig /tmp/kubeconfig --nodes ${TALOS_VIP} --force"
  exit 1
fi

echo "==> Using kubeconfig from ${KUBECONFIG_SRC}"

# Verify connectivity
export KUBECONFIG="${KUBECONFIG_SRC}"
kubectl get nodes --request-timeout=5s >/dev/null 2>&1 || {
  echo "ERROR: Cannot reach the cluster with this kubeconfig."
  echo "       Ensure Talos VIP (${TALOS_VIP}) is reachable."
  exit 1
}
echo "==> Cluster reachable."

# ---- Step 2: Install kubeconfig for Teleport ----
echo ""
echo "==> [Step 2] Installing kubeconfig for Teleport..."
sudo mkdir -p /etc/teleport
sudo cp "${KUBECONFIG_SRC}" /etc/teleport/kubeconfig
sudo chown root:root /etc/teleport/kubeconfig
sudo chmod 600 /etc/teleport/kubeconfig

# Rename the kubeconfig context to match the kube_cluster resource name.
# talosctl generates contexts like "admin@waddle-cluster" -- Teleport's kubernetes_service
# matches kube_cluster resources to kubeconfig entries by context name.
CURRENT_CTX=$(sudo kubectl --kubeconfig=/etc/teleport/kubeconfig config current-context)
if [ "${CURRENT_CTX}" != "${KUBE_CLUSTER_NAME}" ]; then
  echo "    Renaming kubeconfig context '${CURRENT_CTX}' -> '${KUBE_CLUSTER_NAME}'"
  sudo kubectl --kubeconfig=/etc/teleport/kubeconfig config rename-context "${CURRENT_CTX}" "${KUBE_CLUSTER_NAME}"
fi

# ---- Step 3: Add kube_listen_addr + kube_public_addr to proxy_service ----
# Teleport 18.x with ACME does NOT register the teleport-kube ALPN handler on the
# web port (3080). Kube traffic must go through a dedicated kube listener (3033).
echo ""
echo "==> [Step 3] Updating proxy_service with kube listener..."

if sudo grep -q 'kube_listen_addr' /etc/teleport.yaml; then
  echo "    kube_listen_addr already present, skipping."
else
  sudo sed -i '/^proxy_service:/,/^[a-z]/ {
    /^  acme:/i\  kube_listen_addr: 0.0.0.0:3033\n  kube_public_addr: teleport.waddle.social:3033
  }' /etc/teleport.yaml
  echo "    kube_listen_addr and kube_public_addr added to proxy_service."
fi

# ---- Step 4: Add kubernetes_service to teleport.yaml ----
echo ""
echo "==> [Step 4] Adding kubernetes_service..."

if sudo grep -q 'kubernetes_service' /etc/teleport.yaml; then
  echo "    kubernetes_service already present, skipping."
else
  sudo tee -a /etc/teleport.yaml > /dev/null <<'EOF'
kubernetes_service:
  enabled: true
  listen_addr: 0.0.0.0:3026
  kubeconfig_file: /etc/teleport/kubeconfig
  resources:
    - labels:
        "*": "*"
EOF
  echo "    kubernetes_service added."
fi

# ---- Step 5: Restart Teleport ----
echo ""
echo "==> [Step 5] Restarting Teleport..."
sudo systemctl restart teleport
sleep 5

# Verify Teleport is healthy
if sudo systemctl is-active --quiet teleport; then
  echo "    Teleport is running."
else
  echo "ERROR: Teleport failed to start. Check: sudo journalctl -u teleport --no-pager -n 30"
  exit 1
fi

# ---- Step 6: Register kube_cluster resource ----
echo ""
echo "==> [Step 6] Registering kube_cluster resource..."

if sudo tctl get kube_cluster/waddle 2>/dev/null | grep -q 'waddle'; then
  echo "    kube_cluster 'waddle' already exists, skipping."
else
  sudo tctl create -f <<'RESOURCE'
kind: kube_cluster
version: v3
metadata:
  name: waddle
  labels:
    env: production
    cluster: waddle
spec: {}
RESOURCE
  echo "    kube_cluster 'waddle' created."
fi

# ---- Step 7: Verify ----
echo ""
echo "==> [Step 7] Verifying..."
echo "    Waiting for kubernetes service to pick up cluster..."
sleep 10

sudo tctl get kube_cluster 2>/dev/null && echo "    Kubernetes cluster registered in Teleport." || {
  echo "    WARNING: Cluster not yet visible. It may take a few more seconds."
  echo "    Check with: sudo tctl get kube_cluster"
}

echo ""
echo "=========================================="
echo "  Phase 7 complete!"
echo "=========================================="
echo ""
echo "IMPORTANT: Ensure port 3033 is forwarded on the Proxmox host:"
echo "  iptables -t nat -A PREROUTING -i vmbr0 -p tcp --dport 3033 -j DNAT --to-destination 10.10.0.2:3033"
echo "  iptables -A FORWARD -i vmbr0 -o vmbr1 -p tcp --dport 3033 -j ACCEPT"
echo "  netfilter-persistent save"
echo ""
echo "From your workstation:"
echo "  tsh login --proxy=teleport.waddle.social --auth=github"
echo "  tsh kube ls"
echo "  tsh kube login waddle"
echo "  kubectl get nodes"
