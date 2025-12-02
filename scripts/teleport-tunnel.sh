#!/bin/bash
# =============================================================================
# teleport-tunnel.sh - Start TCP tunnels through Teleport for Talos provisioning
# =============================================================================
#
# This script establishes TCP tunnels through Teleport to reach internal
# Talos nodes for cluster bootstrapping.
#
# Prerequisites:
#   1. Teleport CLI installed: brew install teleport
#   2. Teleport server configured with TCP apps for Talos nodes
#   3. Either: Interactive login (tsh login) or Machine ID (tbot)
#
# Usage:
#   source ./scripts/teleport-tunnel.sh    # Start tunnels
#   ./scripts/teleport-tunnel.sh stop      # Stop tunnels
#
# Environment Variables:
#   TELEPORT_PROXY     - Teleport proxy address (default: teleport.waddle.social:443)
#   TALOS_CP_COUNT     - Number of control plane nodes (default: 3)
#   TBOT_CONFIG        - Path to tbot config for Machine ID (optional)
#
# =============================================================================

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
TELEPORT_PROXY="${TELEPORT_PROXY:-teleport.waddle.social:443}"
TALOS_CP_COUNT="${TALOS_CP_COUNT:-3}"
TBOT_CONFIG="${TBOT_CONFIG:-$HOME/.tbot/config.yaml}"

# Port mappings
# Talos API: 50001-50003 -> internal 192.168.1.101-103:50000
# Kubernetes API: 6443 -> internal 192.168.1.101:6443
TALOS_PORT_START=50001
K8S_API_PORT=6443

# PID file for tracking tunnel processes
PID_FILE="/tmp/teleport-tunnels.pid"

# Function to check if tsh is installed
check_tsh() {
    if ! command -v tsh &> /dev/null; then
        echo -e "${RED}Error: Teleport CLI (tsh) is not installed${NC}"
        echo ""
        echo "Install it from: https://goteleport.com/docs/installation/"
        echo ""
        echo "  macOS:   brew install teleport"
        echo "  Linux:   See https://goteleport.com/docs/installation/"
        exit 1
    fi
}

# Function to check Teleport login status
check_login() {
    if ! tsh status &> /dev/null 2>&1; then
        echo -e "${YELLOW}Not logged into Teleport. Attempting login...${NC}"
        
        # Check if Machine ID (tbot) is configured
        if [[ -f "$TBOT_CONFIG" ]]; then
            echo -e "${BLUE}Using Machine ID for authentication...${NC}"
            if command -v tbot &> /dev/null; then
                tbot start -c "$TBOT_CONFIG" --oneshot 2>/dev/null || true
            fi
        fi
        
        # Try interactive login
        if ! tsh status &> /dev/null 2>&1; then
            echo -e "${BLUE}Starting interactive login...${NC}"
            tsh login --proxy="$TELEPORT_PROXY"
        fi
    fi
    
    echo -e "${GREEN}Logged into Teleport${NC}"
}

# Function to start tunnels
start_tunnels() {
    echo -e "${BLUE}Starting Teleport TCP tunnels...${NC}"
    
    # Kill any existing tunnels
    stop_tunnels 2>/dev/null || true
    
    # Clear PID file
    > "$PID_FILE"
    
    # Start Talos API tunnels
    for i in $(seq 1 "$TALOS_CP_COUNT"); do
        local_port=$((TALOS_PORT_START + i - 1))
        app_name="talos-cp${i}"
        
        echo -e "  Starting tunnel: ${app_name} -> localhost:${local_port}"
        tsh proxy app "$app_name" -p "$local_port" &
        echo $! >> "$PID_FILE"
        sleep 0.5
    done
    
    # Start Kubernetes API tunnel
    echo -e "  Starting tunnel: kubernetes-api -> localhost:${K8S_API_PORT}"
    tsh proxy app kubernetes-api -p "$K8S_API_PORT" &
    echo $! >> "$PID_FILE"
    
    # Wait for tunnels to establish
    sleep 2
    
    echo ""
    echo -e "${GREEN}Tunnels established:${NC}"
    for i in $(seq 1 "$TALOS_CP_COUNT"); do
        local_port=$((TALOS_PORT_START + i - 1))
        echo -e "  ${GREEN}✓${NC} Talos CP${i}: localhost:${local_port}"
    done
    echo -e "  ${GREEN}✓${NC} Kubernetes API: localhost:${K8S_API_PORT}"
    echo ""
    
    # Export environment variables for tunnel mode
    export TALOS_USE_TUNNEL=true
    export TALOS_TUNNEL_HOST=127.0.0.1
    export TALOS_TUNNEL_PORT_START=$TALOS_PORT_START
    export KUBECONFIG_ENDPOINT="https://127.0.0.1:${K8S_API_PORT}"
}

# Function to stop tunnels
stop_tunnels() {
    echo -e "${BLUE}Stopping Teleport tunnels...${NC}"
    
    # Kill processes from PID file
    if [[ -f "$PID_FILE" ]]; then
        while read -r pid; do
            if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
                kill "$pid" 2>/dev/null || true
            fi
        done < "$PID_FILE"
        rm -f "$PID_FILE"
    fi
    
    # Also kill any remaining tsh proxy processes
    pkill -f "tsh proxy app" 2>/dev/null || true
    
    echo -e "${GREEN}Tunnels stopped${NC}"
}

# Function to check tunnel status
status_tunnels() {
    echo -e "${BLUE}Teleport Tunnel Status:${NC}"
    echo ""
    
    if [[ -f "$PID_FILE" ]] && [[ -s "$PID_FILE" ]]; then
        local active=0
        while read -r pid; do
            if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
                ((active++))
            fi
        done < "$PID_FILE"
        
        if [[ $active -gt 0 ]]; then
            echo -e "  ${GREEN}Active tunnels: ${active}${NC}"
            echo ""
            echo "  Checking connectivity..."
            
            for i in $(seq 1 "$TALOS_CP_COUNT"); do
                local_port=$((TALOS_PORT_START + i - 1))
                if nc -z 127.0.0.1 "$local_port" 2>/dev/null; then
                    echo -e "    ${GREEN}✓${NC} localhost:${local_port} (Talos CP${i})"
                else
                    echo -e "    ${RED}✗${NC} localhost:${local_port} (Talos CP${i})"
                fi
            done
            
            if nc -z 127.0.0.1 "$K8S_API_PORT" 2>/dev/null; then
                echo -e "    ${GREEN}✓${NC} localhost:${K8S_API_PORT} (Kubernetes API)"
            else
                echo -e "    ${RED}✗${NC} localhost:${K8S_API_PORT} (Kubernetes API)"
            fi
        else
            echo -e "  ${YELLOW}No active tunnels${NC}"
        fi
    else
        echo -e "  ${YELLOW}No tunnels running${NC}"
    fi
}

# Main execution
main() {
    local command="${1:-start}"
    
    case "$command" in
        start)
            check_tsh
            check_login
            start_tunnels
            ;;
        stop)
            stop_tunnels
            ;;
        status)
            status_tunnels
            ;;
        restart)
            stop_tunnels
            sleep 1
            check_tsh
            check_login
            start_tunnels
            ;;
        *)
            echo "Usage: $0 {start|stop|status|restart}"
            exit 1
            ;;
    esac
}

# Run main if not being sourced
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
