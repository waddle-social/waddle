#!/bin/bash
# =============================================================================
# deploy-via-teleport.sh - Deploy infrastructure through Teleport tunnels
# =============================================================================
#
# This script provides a complete deployment workflow that:
# 1. Logs into Teleport (interactive or Machine ID)
# 2. Establishes TCP tunnels to internal Talos nodes
# 3. Runs CDKTF deployment with tunnel mode enabled
# 4. Cleans up tunnels on exit
#
# Prerequisites:
#   1. Teleport CLI installed: brew install teleport
#   2. Teleport server configured with TCP apps for Talos nodes
#   3. 1Password CLI for secrets (optional): brew install --cask 1password-cli
#
# Usage:
#   ./scripts/deploy-via-teleport.sh           # Full deployment
#   ./scripts/deploy-via-teleport.sh synth     # Synthesize only
#   ./scripts/deploy-via-teleport.sh destroy   # Destroy infrastructure
#
# Environment Variables:
#   TELEPORT_PROXY     - Teleport proxy address (default: teleport.waddle.social:443)
#   SKIP_TUNNELS       - Skip tunnel setup if already running (default: false)
#   USE_1PASSWORD      - Use 1Password for secrets (default: true if op installed)
#
# =============================================================================

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Configuration
TELEPORT_PROXY="${TELEPORT_PROXY:-teleport.waddle.social:443}"
SKIP_TUNNELS="${SKIP_TUNNELS:-false}"
USE_1PASSWORD="${USE_1PASSWORD:-auto}"

# Determine action
ACTION="${1:-deploy}"

# Cleanup function
cleanup() {
    echo ""
    echo -e "${BLUE}Cleaning up...${NC}"
    "$SCRIPT_DIR/teleport-tunnel.sh" stop 2>/dev/null || true
}

# Set trap for cleanup on exit
trap cleanup EXIT INT TERM

# Print header
echo ""
echo -e "${BLUE}=========================================${NC}"
echo -e "${BLUE}  Waddle Infrastructure Deployment${NC}"
echo -e "${BLUE}  via Teleport Secure Tunnels${NC}"
echo -e "${BLUE}=========================================${NC}"
echo ""

# Check Teleport CLI
if ! command -v tsh &> /dev/null; then
    echo -e "${RED}Error: Teleport CLI (tsh) is not installed${NC}"
    echo "Install with: brew install teleport"
    exit 1
fi

# Login to Teleport if needed
echo -e "${BLUE}[1/4] Checking Teleport authentication...${NC}"
if ! tsh status &> /dev/null 2>&1; then
    echo -e "${YELLOW}Logging into Teleport...${NC}"
    tsh login --proxy="$TELEPORT_PROXY"
fi
echo -e "${GREEN}✓ Authenticated to Teleport${NC}"
echo ""

# Start tunnels
if [[ "$SKIP_TUNNELS" != "true" ]]; then
    echo -e "${BLUE}[2/4] Establishing TCP tunnels...${NC}"
    source "$SCRIPT_DIR/teleport-tunnel.sh"
    start_tunnels
else
    echo -e "${YELLOW}[2/4] Skipping tunnel setup (SKIP_TUNNELS=true)${NC}"
    export TALOS_USE_TUNNEL=true
    export TALOS_TUNNEL_HOST=127.0.0.1
fi
echo ""

# Set environment for tunnel mode
export TALOS_USE_TUNNEL=true
export TALOS_TUNNEL_HOST=127.0.0.1

# Determine if using 1Password
if [[ "$USE_1PASSWORD" == "auto" ]]; then
    if command -v op &> /dev/null; then
        USE_1PASSWORD="true"
    else
        USE_1PASSWORD="false"
    fi
fi

# Run deployment
echo -e "${BLUE}[3/4] Running CDKTF ${ACTION}...${NC}"
echo ""

cd "$PROJECT_ROOT"

case "$ACTION" in
    synth)
        if [[ "$USE_1PASSWORD" == "true" ]]; then
            "$SCRIPT_DIR/run-with-secrets.sh" bun run synth
        else
            bun run synth
        fi
        ;;
    deploy)
        if [[ "$USE_1PASSWORD" == "true" ]]; then
            "$SCRIPT_DIR/run-with-secrets.sh" bun run deploy
        else
            bun run deploy
        fi
        ;;
    destroy)
        echo -e "${RED}WARNING: This will destroy all infrastructure!${NC}"
        read -p "Are you sure? (yes/no): " confirm
        if [[ "$confirm" == "yes" ]]; then
            if [[ "$USE_1PASSWORD" == "true" ]]; then
                "$SCRIPT_DIR/run-with-secrets.sh" bun run destroy
            else
                bun run destroy
            fi
        else
            echo "Aborted."
            exit 0
        fi
        ;;
    *)
        echo -e "${RED}Unknown action: $ACTION${NC}"
        echo "Usage: $0 {synth|deploy|destroy}"
        exit 1
        ;;
esac

echo ""
echo -e "${BLUE}[4/4] Deployment complete${NC}"
echo ""
echo -e "${GREEN}=========================================${NC}"
echo -e "${GREEN}  Deployment finished successfully!${NC}"
echo -e "${GREEN}=========================================${NC}"
echo ""

# Show next steps
if [[ "$ACTION" == "deploy" ]]; then
    echo -e "${BLUE}Next steps:${NC}"
    echo "  1. Verify VMs in Proxmox web UI"
    echo "  2. Check Talos cluster health:"
    echo "     talosctl --talosconfig=./talosconfig health"
    echo "  3. Get kubeconfig:"
    echo "     talosctl --talosconfig=./talosconfig kubeconfig ./kubeconfig"
    echo ""
fi
