#!/usr/bin/env bash
set -euo pipefail

# Phase 7: Configure Teleport Kubernetes Access and Talos API tunneling
#
# Prerequisites:
#   - Teleport is running and accessible at teleport.waddle.social
#   - Talos cluster is bootstrapped with VIP at 10.10.0.20
#   - You are logged into Teleport: tsh login --proxy=teleport.waddle.social
#   - You have SSH access to the Teleport VM via Teleport

TELEPORT_VM_IP="10.10.0.2"
TALOS_VIP="10.10.0.20"
TALOS_NODE_IPS=("10.10.0.10" "10.10.0.11" "10.10.0.12")

echo "=========================================="
echo "  Phase 7: Teleport K8s + Talos Setup"
echo "=========================================="

# ---- Step 1: Generate a kubeconfig for Teleport ----
echo ""
echo "==> [Step 1] Generating kubeconfig for Teleport kube agent..."
echo "    The Teleport VM needs a kubeconfig to connect to the Talos cluster."
echo ""

KUBECONFIG_DIR="/tmp/teleport-kube"
mkdir -p "${KUBECONFIG_DIR}"

# Generate a kubeconfig pointing at the Talos VIP
# This needs to run from a machine that already has talosctl access
export TALOSCONFIG="${TALOSCONFIG:-talos/generated/talosconfig}"
talosctl kubeconfig "${KUBECONFIG_DIR}/kubeconfig" \
  --nodes "${TALOS_VIP}" \
  --force 2>/dev/null || {
  echo "ERROR: Could not generate kubeconfig. Ensure TALOSCONFIG is set"
  echo "       and you have access to the Talos cluster."
  exit 1
}

echo "    Kubeconfig generated at ${KUBECONFIG_DIR}/kubeconfig"

# ---- Step 2: Copy kubeconfig to Teleport VM ----
echo ""
echo "==> [Step 2] Copying kubeconfig to Teleport VM..."
echo "    Using tsh scp to transfer through Teleport tunnel."
echo ""

tsh scp "${KUBECONFIG_DIR}/kubeconfig" "deploy@teleport:/tmp/kubeconfig" || {
  echo "ERROR: Could not scp to Teleport VM."
  echo "       Make sure you are logged into Teleport: tsh login --proxy=teleport.waddle.social"
  exit 1
}

# ---- Step 3: Configure Teleport to use the kubeconfig ----
echo ""
echo "==> [Step 3] Configuring Teleport kubernetes_service..."
echo "    SSHing into the Teleport VM to move kubeconfig and restart Teleport."
echo ""

tsh ssh deploy@teleport << 'REMOTE_COMMANDS'
sudo mkdir -p /etc/teleport
sudo mv /tmp/kubeconfig /etc/teleport/kubeconfig
sudo chown root:root /etc/teleport/kubeconfig
sudo chmod 600 /etc/teleport/kubeconfig

# Update teleport.yaml to reference the kubeconfig
sudo python3 -c "
import yaml
with open('/etc/teleport.yaml', 'r') as f:
    config = yaml.safe_load(f)
config['kubernetes_service']['kubeconfig_path'] = '/etc/teleport/kubeconfig'
with open('/etc/teleport.yaml', 'w') as f:
    yaml.dump(config, f, default_flow_style=False)
" 2>/dev/null || {
  # Fallback: use sed if python3-yaml not available
  sudo sed -i 's|kubeconfig_path: ""|kubeconfig_path: "/etc/teleport/kubeconfig"|' /etc/teleport.yaml
}

sudo systemctl restart teleport
echo "Teleport restarted with kubernetes_service kubeconfig."
REMOTE_COMMANDS

echo ""
echo "==> [Step 4] Verifying Teleport Kubernetes Access..."
sleep 5

tsh kube ls 2>/dev/null && echo "    Kubernetes cluster visible in Teleport." || {
  echo "    Waiting 10 more seconds for Teleport to register the cluster..."
  sleep 10
  tsh kube ls
}

echo ""
echo "==> [Step 5] Testing kubectl through Teleport..."
tsh kube login waddle-cluster
kubectl get nodes

echo ""
echo "=========================================="
echo "  Teleport Kubernetes Access configured!"
echo "=========================================="
echo ""
echo "Usage:"
echo "  tsh login --proxy=teleport.waddle.social"
echo "  tsh kube login waddle-cluster"
echo "  kubectl get nodes"
echo ""
echo "For talosctl via Teleport tunnel:"
echo "  tsh proxy kube --port=6443 &"
echo "  talosctl --endpoints 127.0.0.1 --nodes <node-ip> get members"
echo ""
echo "For SSH to the Proxmox host:"
echo "  tsh ssh root@proxmox-host"
echo ""

# Cleanup
rm -rf "${KUBECONFIG_DIR}"
