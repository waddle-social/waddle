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

TALOS_VIP="10.10.0.20"
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

# ---- Step 3: Add kubernetes_service to teleport.yaml ----
echo ""
echo "==> [Step 3] Updating /etc/teleport.yaml..."

if sudo grep -q 'kubernetes_service' /etc/teleport.yaml; then
  echo "    kubernetes_service already present, skipping."
else
  sudo tee -a /etc/teleport.yaml > /dev/null <<'EOF'
kubernetes_service:
  enabled: true
  listen_addr: 0.0.0.0:3026
  kubeconfig_file: /etc/teleport/kubeconfig
  labels:
    env: production
    cluster: waddle
EOF
  echo "    kubernetes_service added."
fi

# ---- Step 4: Restart Teleport ----
echo ""
echo "==> [Step 4] Restarting Teleport..."
sudo systemctl restart teleport
sleep 5

# Verify Teleport is healthy
if sudo systemctl is-active --quiet teleport; then
  echo "    Teleport is running."
else
  echo "ERROR: Teleport failed to start. Check: sudo journalctl -u teleport --no-pager -n 30"
  exit 1
fi

# ---- Step 5: Verify ----
echo ""
echo "==> [Step 5] Verifying..."
echo "    Waiting for kubernetes service to register..."
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
echo "From your workstation:"
echo "  tsh login --proxy=teleport.waddle.social --auth=github"
echo "  tsh kube ls"
echo "  tsh kube login waddle"
echo "  kubectl get nodes"
