#!/usr/bin/env bash
# Emergency Recovery Script
# Run via Scaleway out-of-band console when Teleport is down
# and you need to regain access to the Proxmox host.
#
# WARNING: This temporarily exposes SSH on the public interface.
# Remove the rule as soon as recovery is complete.

set -euo pipefail

echo "=========================================="
echo "  EMERGENCY RECOVERY"
echo "=========================================="
echo ""
echo "This script temporarily opens SSH on the public"
echo "interface. Remove the rule immediately after recovery."
echo ""

read -rp "Your current public IP: " OPERATOR_IP
read -rp "Confirm you want to proceed? (yes/no): " CONFIRM

if [ "${CONFIRM}" != "yes" ]; then
  echo "Aborted."
  exit 1
fi

echo ""
echo "==> Adding temporary SSH access from ${OPERATOR_IP}..."

# Using pve-firewall: add a temporary rule
pvesh create /nodes/$(hostname)/firewall/rules \
  --action ACCEPT \
  --type in \
  --source "${OPERATOR_IP}" \
  --dport 22 \
  --proto tcp \
  --comment "EMERGENCY: Temporary SSH access - REMOVE AFTER RECOVERY" \
  --enable 1 \
  --pos 0

echo ""
echo "==> SSH access enabled from ${OPERATOR_IP}."
echo ""
echo "After recovery, remove the emergency rule:"
echo "  pvesh get /nodes/$(hostname)/firewall/rules"
echo "  pvesh delete /nodes/$(hostname)/firewall/rules/<pos>"
echo ""
echo "Or via Proxmox UI: Datacenter > Node > Firewall > Rules"
echo "=========================================="
