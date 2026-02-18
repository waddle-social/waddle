#!/usr/bin/env bash
set -euo pipefail

# XMPP Compliance Test Runner (Rust orchestrated)
#
# This wrapper delegates to:
#   cargo test -p waddle-xmpp --test xep0479_compliance -- --ignored --nocapture
#
# The Rust harness manages:
# - TLS cert generation (x509),
# - waddle-server lifecycle,
# - interop container execution (testcontainers-rs),
# - artifact collection and summary output.

PROFILE="${WADDLE_COMPLIANCE_PROFILE:-best_effort_full}"
DOMAIN="${WADDLE_COMPLIANCE_DOMAIN:-localhost}"
HOST="${WADDLE_COMPLIANCE_HOST:-host.docker.internal}"
TIMEOUT_MS="${WADDLE_COMPLIANCE_TIMEOUT_MS:-10000}"
ADMIN_USER="${WADDLE_COMPLIANCE_ADMIN_USERNAME:-admin}"
ADMIN_PASS="${WADDLE_COMPLIANCE_ADMIN_PASSWORD:-interop-test-password}"
ENABLED_SPECS="${WADDLE_COMPLIANCE_ENABLED_SPECS:-}"
DISABLED_SPECS="${WADDLE_COMPLIANCE_DISABLED_SPECS:-}"
ARTIFACT_DIR="${WADDLE_COMPLIANCE_ARTIFACT_DIR:-$(pwd)/test-logs}"
KEEP_CONTAINERS="${WADDLE_COMPLIANCE_KEEP_CONTAINERS:-false}"

# Legacy options kept for compatibility with older script callers.
LEGACY_PORT=""
LEGACY_SECURITY_MODE=""
LEGACY_START_SERVER=false

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

print_usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Run XMPP compliance tests through the Rust testcontainers harness.

Options:
  -h, --help                Show this help message
  -d, --domain DOMAIN       XMPP domain (default: $DOMAIN)
  -H, --host HOST           Interop host (default: $HOST)
  -u, --user USER           Admin username (default: $ADMIN_USER)
  -P, --password PASS       Admin password (default: ********)
  -t, --timeout MS          Interop timeout in ms (default: $TIMEOUT_MS)
  -e, --enabled SPECS       Enabled specifications CSV
  -D, --disabled SPECS      Disabled specifications CSV
  -l, --log-dir DIR         Artifact directory (default: $ARTIFACT_DIR)
  --profile PROFILE         best_effort_full | core_strict | full_strict
  --keep-containers         Keep interop container after run

Legacy options (ignored, printed as warnings):
  -p, --port PORT
  -s, --security MODE
  --start-server

Examples:
  $0
  $0 -e 'RFC6120,RFC6121,XEP-0030'
  $0 --profile core_strict
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            print_usage
            exit 0
            ;;
        -d|--domain)
            DOMAIN="$2"
            shift 2
            ;;
        -H|--host)
            HOST="$2"
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
        -t|--timeout)
            TIMEOUT_MS="$2"
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
            ARTIFACT_DIR="$2"
            shift 2
            ;;
        --profile)
            PROFILE="$2"
            shift 2
            ;;
        --keep-containers)
            KEEP_CONTAINERS=true
            shift
            ;;
        -p|--port)
            LEGACY_PORT="$2"
            shift 2
            ;;
        -s|--security)
            LEGACY_SECURITY_MODE="$2"
            shift 2
            ;;
        --start-server)
            LEGACY_START_SERVER=true
            shift
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            print_usage
            exit 1
            ;;
    esac
done

mkdir -p "$ARTIFACT_DIR"

if [[ -n "$LEGACY_PORT" ]]; then
    echo -e "${YELLOW}Warning: --port is ignored by the Rust harness (fixed XMPP port 5222).${NC}"
fi
if [[ -n "$LEGACY_SECURITY_MODE" ]]; then
    echo -e "${YELLOW}Warning: --security is ignored; harness always enforces TLS-required mode.${NC}"
fi
if [[ "$LEGACY_START_SERVER" == "true" ]]; then
    echo -e "${YELLOW}Warning: --start-server is ignored; harness manages server lifecycle automatically.${NC}"
fi

echo -e "${GREEN}XMPP Compliance Test (Rust Harness)${NC}"
echo "======================================="
echo "Profile:        $PROFILE"
echo "Domain:         $DOMAIN"
echo "Host:           $HOST"
echo "Timeout (ms):   $TIMEOUT_MS"
echo "Admin User:     $ADMIN_USER"
echo "Artifacts:      $ARTIFACT_DIR"
echo "Keep Container: $KEEP_CONTAINERS"
if [[ -n "$ENABLED_SPECS" ]]; then
    echo "Enabled Specs:  $ENABLED_SPECS"
fi
if [[ -n "$DISABLED_SPECS" ]]; then
    echo "Disabled Specs: $DISABLED_SPECS"
fi
echo ""

export WADDLE_COMPLIANCE_PROFILE="$PROFILE"
export WADDLE_COMPLIANCE_DOMAIN="$DOMAIN"
export WADDLE_COMPLIANCE_HOST="$HOST"
export WADDLE_COMPLIANCE_TIMEOUT_MS="$TIMEOUT_MS"
export WADDLE_COMPLIANCE_ADMIN_USERNAME="$ADMIN_USER"
export WADDLE_COMPLIANCE_ADMIN_PASSWORD="$ADMIN_PASS"
export WADDLE_COMPLIANCE_ENABLED_SPECS="$ENABLED_SPECS"
export WADDLE_COMPLIANCE_DISABLED_SPECS="$DISABLED_SPECS"
export WADDLE_COMPLIANCE_ARTIFACT_DIR="$ARTIFACT_DIR"
export WADDLE_COMPLIANCE_KEEP_CONTAINERS="$KEEP_CONTAINERS"

cargo test -p waddle-xmpp --test xep0479_compliance -- --ignored --nocapture
