#!/bin/bash
# =============================================================================
# run-with-secrets.sh - Inject secrets from 1Password and run commands
# =============================================================================
#
# Usage:
#   ./scripts/run-with-secrets.sh <command>
#   ./scripts/run-with-secrets.sh bun run deploy
#   ./scripts/run-with-secrets.sh bun run synth
#
# Prerequisites:
#   1. Install 1Password CLI: https://developer.1password.com/docs/cli/get-started
#   2. Sign in to 1Password: op signin
#   3. Create vault "waddle-infra" with required secrets
#
# Environment Variables:
#   OP_ENV_FILE - Path to .env.op file (default: infrastructure/.env.op)
#   OP_ACCOUNT  - 1Password account to use (optional)
#
# =============================================================================

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Default env file location
OP_ENV_FILE="${OP_ENV_FILE:-$PROJECT_ROOT/infrastructure/.env.op}"

# Check if 1Password CLI is installed
if ! command -v op &> /dev/null; then
    echo -e "${RED}Error: 1Password CLI (op) is not installed${NC}"
    echo ""
    echo "Install it from: https://developer.1password.com/docs/cli/get-started"
    echo ""
    echo "  macOS:   brew install --cask 1password-cli"
    echo "  Linux:   See https://developer.1password.com/docs/cli/get-started#install"
    echo "  Windows: winget install AgileBits.1Password.CLI"
    exit 1
fi

# Check if signed in to 1Password
if ! op account list &> /dev/null 2>&1; then
    echo -e "${YELLOW}Warning: Not signed in to 1Password${NC}"
    echo "Running: op signin"
    eval "$(op signin)"
fi

# Check if env file exists
if [[ ! -f "$OP_ENV_FILE" ]]; then
    echo -e "${RED}Error: 1Password env file not found: $OP_ENV_FILE${NC}"
    echo ""
    echo "Create it by copying the template:"
    echo "  cp infrastructure/.env.op.example infrastructure/.env.op"
    exit 1
fi

# Check if command was provided
if [[ $# -eq 0 ]]; then
    echo -e "${RED}Error: No command provided${NC}"
    echo ""
    echo "Usage: $0 <command>"
    echo ""
    echo "Examples:"
    echo "  $0 bun run deploy"
    echo "  $0 bun run synth"
    echo "  $0 cdktf deploy"
    exit 1
fi

echo -e "${GREEN}Loading secrets from 1Password...${NC}"

# Run command with secrets injected
# The --env-file flag loads op:// references and injects them as environment variables
exec op run --env-file="$OP_ENV_FILE" -- "$@"
