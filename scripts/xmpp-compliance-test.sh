#!/usr/bin/env bash
set -euo pipefail

# XMPP Compliance Testing Script
# Uses the XMPP Interop Testing framework (933+ tests) to verify
# XEP-0479 (XMPP Compliance Suites 2023) compliance.
#
# See: https://xmpp-interop-testing.github.io/

# Configuration (can be overridden via environment variables)
XMPP_DOMAIN="${XMPP_DOMAIN:-localhost}"
XMPP_HOST="${XMPP_HOST:-127.0.0.1}"
XMPP_PORT="${XMPP_PORT:-5222}"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-interop-test-password}"
SECURITY_MODE="${SECURITY_MODE:-disabled}"  # "disabled" or "required" for TLS
TIMEOUT="${TIMEOUT:-10000}"
LOG_DIR="${LOG_DIR:-$(pwd)/test-logs}"

# Default disabled specifications (not yet implemented)
DEFAULT_DISABLED_SPECS="XEP-0220,XEP-0060,XEP-0163,XEP-0363,XEP-0054,XEP-0191"
DISABLED_SPECS="${DISABLED_SPECS:-$DEFAULT_DISABLED_SPECS}"

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

print_usage() {
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Run XMPP Interop Tests against the Waddle XMPP server."
    echo ""
    echo "Options:"
    echo "  -h, --help              Show this help message"
    echo "  -d, --domain DOMAIN     XMPP domain (default: localhost)"
    echo "  -H, --host HOST         Server hostname (default: 127.0.0.1)"
    echo "  -p, --port PORT         XMPP port (default: 5222)"
    echo "  -u, --user USER         Admin username (default: admin)"
    echo "  -P, --password PASS     Admin password (default: interop-test-password)"
    echo "  -s, --security MODE     Security mode: disabled or required (default: disabled)"
    echo "  -e, --enabled SPECS     Only run specific XEPs (comma-separated)"
    echo "  -D, --disabled SPECS    Skip specific XEPs (comma-separated)"
    echo "  -l, --log-dir DIR       Log directory (default: ./test-logs)"
    echo "  --start-server          Start the Waddle server before testing"
    echo ""
    echo "Environment Variables:"
    echo "  XMPP_DOMAIN, XMPP_HOST, XMPP_PORT, ADMIN_USER, ADMIN_PASS"
    echo "  SECURITY_MODE, DISABLED_SPECS, ENABLED_SPECS, LOG_DIR"
    echo ""
    echo "Examples:"
    echo "  # Run all tests with defaults"
    echo "  $0"
    echo ""
    echo "  # Test only specific XEPs"
    echo "  $0 -e 'XEP-0030,XEP-0198,XEP-0280'"
    echo ""
    echo "  # Start server and run tests"
    echo "  $0 --start-server"
}

START_SERVER=false
ENABLED_SPECS=""

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -h|--help)
            print_usage
            exit 0
            ;;
        -d|--domain)
            XMPP_DOMAIN="$2"
            shift 2
            ;;
        -H|--host)
            XMPP_HOST="$2"
            shift 2
            ;;
        -p|--port)
            XMPP_PORT="$2"
            shift 2
            ;;
        -u|--user)
            ADMIN_USER="$2"
            shift 2
            ;;
        -P|--password)
            ADMIN_PASS="$2"
            shift 2
            ;;
        -s|--security)
            SECURITY_MODE="$2"
            shift 2
            ;;
        -e|--enabled)
            ENABLED_SPECS="$2"
            shift 2
            ;;
        -D|--disabled)
            DISABLED_SPECS="$2"
            shift 2
            ;;
        -l|--log-dir)
            LOG_DIR="$2"
            shift 2
            ;;
        --start-server)
            START_SERVER=true
            shift
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            print_usage
            exit 1
            ;;
    esac
done

# Ensure log directory exists
mkdir -p "$LOG_DIR"

echo -e "${GREEN}XMPP Compliance Testing${NC}"
echo "================================"
echo "Domain:        $XMPP_DOMAIN"
echo "Host:          $XMPP_HOST"
echo "Port:          $XMPP_PORT"
echo "Admin User:    $ADMIN_USER"
echo "Security Mode: $SECURITY_MODE"
echo "Log Directory: $LOG_DIR"
echo ""

# Check if Docker is available
if ! command -v docker &> /dev/null; then
    echo -e "${RED}Error: Docker is not installed or not in PATH${NC}"
    exit 1
fi

# Start server if requested
SERVER_PID=""
cleanup() {
    if [[ -n "$SERVER_PID" ]]; then
        echo -e "${YELLOW}Stopping server (PID: $SERVER_PID)...${NC}"
        kill "$SERVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

if [[ "$START_SERVER" == "true" ]]; then
    echo -e "${YELLOW}Starting Waddle server...${NC}"

    # Build if necessary
    if [[ ! -f "target/release/waddle-server" ]]; then
        echo "Building server..."
        cargo build --release --package waddle-server
    fi

    # Generate test certificates if needed
    if [[ ! -f "certs/server.crt" ]]; then
        echo "Generating test certificates..."
        mkdir -p certs
        openssl req -x509 -newkey rsa:4096 -keyout certs/server.key -out certs/server.crt \
            -days 365 -nodes -subj "/CN=localhost" 2>/dev/null
    fi

    # Start server
    RUST_LOG=debug \
    WADDLE_DOMAIN="$XMPP_DOMAIN" \
    WADDLE_TLS_CERT="./certs/server.crt" \
    WADDLE_TLS_KEY="./certs/server.key" \
    WADDLE_ADMIN_JID="$ADMIN_USER@$XMPP_DOMAIN" \
    WADDLE_ADMIN_PASSWORD="$ADMIN_PASS" \
    ./target/release/waddle-server &
    SERVER_PID=$!

    echo "Waiting for server to start..."
    for i in {1..20}; do
        if nc -z "$XMPP_HOST" "$XMPP_PORT" 2>/dev/null; then
            echo -e "${GREEN}Server is ready on port $XMPP_PORT${NC}"
            break
        fi
        if [[ $i -eq 20 ]]; then
            echo -e "${RED}Server failed to start within timeout${NC}"
            exit 1
        fi
        echo "Waiting... attempt $i/20"
        sleep 1
    done
    echo ""
fi

# Build Docker run command
DOCKER_CMD=(
    docker run --rm --network=host
    -v "$LOG_DIR:/logs"
    ghcr.io/xmpp-interop-testing/xmpp_interop_tests:latest
    "--domain=$XMPP_DOMAIN"
    "--host=$XMPP_HOST"
    "--timeout=$TIMEOUT"
    "--adminAccountUsername=$ADMIN_USER"
    "--adminAccountPassword=$ADMIN_PASS"
    "--securityMode=$SECURITY_MODE"
    "--logDir=/logs"
)

# Add enabled/disabled specifications
if [[ -n "$ENABLED_SPECS" ]]; then
    DOCKER_CMD+=("--enabledSpecifications=$ENABLED_SPECS")
elif [[ -n "$DISABLED_SPECS" ]]; then
    DOCKER_CMD+=("--disabledSpecifications=$DISABLED_SPECS")
fi

echo -e "${YELLOW}Running XMPP Interop Tests...${NC}"
echo "Command: ${DOCKER_CMD[*]}"
echo ""

# Run tests
if "${DOCKER_CMD[@]}"; then
    echo ""
    echo -e "${GREEN}Tests completed successfully!${NC}"
    EXIT_CODE=0
else
    echo ""
    echo -e "${YELLOW}Tests completed with failures (expected for unimplemented features)${NC}"
    EXIT_CODE=1
fi

echo ""
echo "Test logs available at: $LOG_DIR"
echo ""

exit $EXIT_CODE
