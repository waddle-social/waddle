#!/bin/bash
#
# Generate development TLS certificates for XMPP server.
#
# Usage:
#   ./scripts/generate-dev-certs.sh [domain]
#
# Arguments:
#   domain - The domain for the certificate (default: localhost)
#
# Output:
#   certs/server.crt - The self-signed certificate
#   certs/server.key - The private key (PKCS#8 format)
#
# Note: These certificates are for development only.
# For production, use proper CA-signed certificates.

set -euo pipefail

DOMAIN="${1:-localhost}"
CERT_DIR="certs"
DAYS_VALID=365

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Generating development TLS certificates for XMPP server${NC}"
echo "Domain: $DOMAIN"
echo "Output directory: $CERT_DIR"
echo ""

# Create certs directory if it doesn't exist
mkdir -p "$CERT_DIR"

# Check if certificates already exist
if [[ -f "$CERT_DIR/server.crt" && -f "$CERT_DIR/server.key" ]]; then
    echo -e "${YELLOW}Warning: Certificates already exist in $CERT_DIR${NC}"
    read -p "Overwrite existing certificates? [y/N] " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi
fi

# Generate private key (RSA 4096-bit)
echo "Generating RSA private key..."
openssl genrsa -out "$CERT_DIR/server.key.tmp" 4096 2>/dev/null

# Convert to PKCS#8 format (required by rustls)
echo "Converting key to PKCS#8 format..."
openssl pkcs8 -topk8 -inform PEM -outform PEM -nocrypt \
    -in "$CERT_DIR/server.key.tmp" \
    -out "$CERT_DIR/server.key"
rm "$CERT_DIR/server.key.tmp"

# Generate self-signed certificate
echo "Generating self-signed certificate..."
openssl req -x509 -new -nodes \
    -key "$CERT_DIR/server.key" \
    -sha256 \
    -days "$DAYS_VALID" \
    -out "$CERT_DIR/server.crt" \
    -subj "/CN=$DOMAIN" \
    -addext "subjectAltName=DNS:$DOMAIN,DNS:muc.$DOMAIN,DNS:*.$DOMAIN"

# Set appropriate permissions
chmod 600 "$CERT_DIR/server.key"
chmod 644 "$CERT_DIR/server.crt"

echo ""
echo -e "${GREEN}Certificates generated successfully!${NC}"
echo ""
echo "Certificate: $CERT_DIR/server.crt"
echo "Private key: $CERT_DIR/server.key"
echo ""
echo "Certificate details:"
openssl x509 -in "$CERT_DIR/server.crt" -noout -subject -dates -ext subjectAltName 2>/dev/null || \
    openssl x509 -in "$CERT_DIR/server.crt" -noout -subject -dates

echo ""
echo -e "${YELLOW}Note: These are self-signed certificates for development only.${NC}"
echo "For production, use certificates from a trusted CA."
echo ""
echo "Environment variables to use these certs:"
echo "  export WADDLE_XMPP_TLS_CERT=$CERT_DIR/server.crt"
echo "  export WADDLE_XMPP_TLS_KEY=$CERT_DIR/server.key"
