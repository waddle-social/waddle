#!/usr/bin/env bash
set -euo pipefail

CLUSTER_NAME="waddle-cluster"
CLUSTER_ENDPOINT="https://10.10.0.20:6443"
NODE_IPS=("10.10.0.10" "10.10.0.11" "10.10.0.12")
NODE_NAMES=("talos-cp1" "talos-cp2" "talos-cp3")
VIP="10.10.0.20"
GATEWAY="10.10.0.1"
CONFIG_DIR="$(cd "$(dirname "$0")/../talos" && pwd)"
OUTPUT_DIR="${CONFIG_DIR}/generated"

echo "==> Generating Talos machine configs..."
mkdir -p "${OUTPUT_DIR}"

talosctl gen config "${CLUSTER_NAME}" "${CLUSTER_ENDPOINT}" \
  --output "${OUTPUT_DIR}" \
  --with-docs=false \
  --with-examples=false \
  --force

echo "==> Generating per-node patched configs..."
for i in "${!NODE_IPS[@]}"; do
  NODE_IP="${NODE_IPS[$i]}"
  NODE_NAME="${NODE_NAMES[$i]}"

  cat > "${OUTPUT_DIR}/${NODE_NAME}-patch.yaml" <<EOF
machine:
  network:
    hostname: ${NODE_NAME}
    interfaces:
      - interface: eth0
        addresses:
          - ${NODE_IP}/24
        routes:
          - network: 0.0.0.0/0
            gateway: ${GATEWAY}
        vip:
          ip: ${VIP}
    nameservers:
      - 1.1.1.1
      - 8.8.8.8
  kubelet:
    nodeIP:
      validSubnets:
        - 10.10.0.0/24
cluster:
  controlPlane:
    endpoint: ${CLUSTER_ENDPOINT}
  network:
    cni:
      name: none
  proxy:
    disabled: true
  allowSchedulingOnControlPlanes: true
  etcd:
    advertisedSubnets:
      - 10.10.0.0/24
EOF

  echo "  - Generated patch for ${NODE_NAME} (${NODE_IP})"
done

echo ""
echo "==> Applying configs to nodes..."
for i in "${!NODE_IPS[@]}"; do
  NODE_IP="${NODE_IPS[$i]}"
  NODE_NAME="${NODE_NAMES[$i]}"

  echo "  - Applying config to ${NODE_NAME} (${NODE_IP})..."
  talosctl apply-config \
    --insecure \
    --nodes "${NODE_IP}" \
    --file "${OUTPUT_DIR}/controlplane.yaml" \
    --config-patch "@${OUTPUT_DIR}/${NODE_NAME}-patch.yaml"
done

echo ""
echo "==> Waiting 30s for nodes to initialize..."
sleep 30

echo "==> Bootstrapping first node (${NODE_IPS[0]})..."
export TALOSCONFIG="${OUTPUT_DIR}/talosconfig"
talosctl config endpoint "${NODE_IPS[0]}"
talosctl config node "${NODE_IPS[0]}"
talosctl bootstrap

echo ""
echo "==> Waiting 60s for cluster bootstrap..."
sleep 60

echo "==> Retrieving kubeconfig..."
talosctl kubeconfig --force

echo ""
echo "==> Cluster bootstrapped. Nodes will show NotReady until Cilium is installed."
echo "    Run scripts/bootstrap-k8s.sh to install Cilium and other bootstrap components."
echo ""
echo "    talosconfig: ${OUTPUT_DIR}/talosconfig"
kubectl get nodes || true
