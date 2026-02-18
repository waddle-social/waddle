#!/usr/bin/env bash
#
# deploy.sh — Deploy waddle-server to Ubuntu for waddle.chat
#
# Usage: sudo bash deploy.sh
#
# Idempotent: safe to re-run for updates.
# Requires: Ubuntu 22.04/24.04, root, DNS A record for waddle.chat pointing here.
#
set -euo pipefail

# ─── Configuration ───────────────────────────────────────────────────────────
DOMAIN="waddle.chat"
REPO_URL="https://github.com/waddle-social/ws.git"
REPO_BRANCH="main"

INSTALL_DIR="/opt/waddle"
DATA_DIR="/var/lib/waddle"
CERT_DIR="/etc/waddle/certs"
ENV_FILE="/etc/waddle/waddle.env"

SERVICE_USER="waddle"
BINARY_NAME="waddle-server"

# ─── Helpers ─────────────────────────────────────────────────────────────────
info()  { echo -e "\033[1;34m▸\033[0m $*"; }
ok()    { echo -e "\033[1;32m✓\033[0m $*"; }
warn()  { echo -e "\033[1;33m⚠\033[0m $*"; }
die()   { echo -e "\033[1;31m✗\033[0m $*" >&2; exit 1; }

require_root() {
    [[ $EUID -eq 0 ]] || die "This script must be run as root (sudo bash deploy.sh)"
}

# ─── Step 0: Preflight ──────────────────────────────────────────────────────
require_root

info "Deploying ${DOMAIN} — $(date -Iseconds)"

# ─── Step 1: System dependencies ────────────────────────────────────────────
info "Step 1: Installing system dependencies..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq \
    build-essential pkg-config libssl-dev \
    curl git \
    nginx certbot python3-certbot-nginx \
    ufw \
    > /dev/null 2>&1
ok "System dependencies installed"

# ─── Step 2: Create system user & directories ───────────────────────────────
info "Step 2: Creating user '${SERVICE_USER}' and directories..."

if ! id "${SERVICE_USER}" &>/dev/null; then
    useradd --system --shell /usr/sbin/nologin --home-dir "${DATA_DIR}" "${SERVICE_USER}"
    ok "Created system user '${SERVICE_USER}'"
else
    ok "User '${SERVICE_USER}' already exists"
fi

mkdir -p "${DATA_DIR}/uploads" "${CERT_DIR}" "${INSTALL_DIR}" /etc/waddle
chown -R "${SERVICE_USER}:${SERVICE_USER}" "${DATA_DIR}"
chown -R "${SERVICE_USER}:${SERVICE_USER}" "${CERT_DIR}"
ok "Directories ready"

# ─── Step 3: Install Rust toolchain ─────────────────────────────────────────
info "Step 3: Installing Rust toolchain..."

# Ensure swap exists for LTO builds (needs ~4GB)
TOTAL_MEM_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
TOTAL_SWAP_KB=$(grep SwapTotal /proc/meminfo | awk '{print $2}')
TOTAL_AVAILABLE=$(( TOTAL_MEM_KB + TOTAL_SWAP_KB ))
if (( TOTAL_AVAILABLE < 4000000 )); then
    warn "Less than 4GB RAM+swap available. Adding 4GB swap for build..."
    if [[ ! -f /swapfile ]]; then
        fallocate -l 4G /swapfile
        chmod 600 /swapfile
        mkswap /swapfile
        swapon /swapfile
        ok "4GB swap created and activated"
    else
        ok "Swap file already exists"
    fi
fi

if command -v rustup &>/dev/null; then
    rustup update stable --no-self-update 2>/dev/null
    ok "Rust toolchain updated"
else
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal 2>/dev/null
    source "${HOME}/.cargo/env"
    ok "Rust toolchain installed"
fi
# Ensure cargo is on PATH for the rest of the script
export PATH="${HOME}/.cargo/bin:${PATH}"

# ─── Step 4: Clone/update repository ────────────────────────────────────────
info "Step 4: Cloning/updating repository..."

if [[ -d "${INSTALL_DIR}/.git" ]]; then
    cd "${INSTALL_DIR}"
    git fetch origin "${REPO_BRANCH}" --quiet
    git reset --hard "origin/${REPO_BRANCH}" --quiet
    ok "Repository updated"
else
    git clone --branch "${REPO_BRANCH}" --depth 1 "${REPO_URL}" "${INSTALL_DIR}" --quiet
    cd "${INSTALL_DIR}"
    ok "Repository cloned"
fi

# Backup previous binary if it exists
if [[ -f "${INSTALL_DIR}/target/release/${BINARY_NAME}" ]]; then
    cp "${INSTALL_DIR}/target/release/${BINARY_NAME}" "${INSTALL_DIR}/target/release/${BINARY_NAME}.prev"
    ok "Previous binary backed up"
fi

# ─── Step 5: Build release binary ───────────────────────────────────────────
info "Step 5: Building release binary (this may take 10-30 minutes with LTO)..."

PREV_BINARY="${INSTALL_DIR}/target/release/${BINARY_NAME}.prev"

cd "${INSTALL_DIR}"
if ! cargo build --release -p waddle-server 2>&1; then
    warn "Build failed!"
    if [[ -f "${PREV_BINARY}" ]]; then
        warn "Restoring previous binary..."
        cp "${PREV_BINARY}" "${INSTALL_DIR}/target/release/${BINARY_NAME}"
        ok "Previous binary restored — deploy aborted but service can restart with old version"
    fi
    die "Build failed — fix errors and re-run deploy.sh"
fi

[[ -f "${INSTALL_DIR}/target/release/${BINARY_NAME}" ]] || die "Build produced no binary"
ok "Binary built: $(ls -lh "${INSTALL_DIR}/target/release/${BINARY_NAME}" | awk '{print $5}')"

# ─── Step 6: nginx bootstrap vhost (HTTP-only for certbot) ──────────────────
info "Step 6: Setting up nginx bootstrap vhost..."

NGINX_BOOTSTRAP="/etc/nginx/sites-available/${DOMAIN}-bootstrap"
cat > "${NGINX_BOOTSTRAP}" <<NGINX_BOOT
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN};

    location /.well-known/acme-challenge/ {
        root /var/www/html;
    }

    location / {
        return 444;
    }
}
NGINX_BOOT

# Remove default site if it exists
rm -f /etc/nginx/sites-enabled/default

ln -sf "${NGINX_BOOTSTRAP}" /etc/nginx/sites-enabled/
# Remove full config temporarily if it exists (certbot needs the bootstrap)
rm -f "/etc/nginx/sites-enabled/${DOMAIN}"
nginx -t 2>/dev/null && systemctl reload nginx
ok "Bootstrap vhost active"

# ─── Step 7: Obtain LetsEncrypt certificates ────────────────────────────────
info "Step 7: Obtaining LetsEncrypt certificates..."

if [[ -d "/etc/letsencrypt/live/${DOMAIN}" ]]; then
    ok "Certificates already exist, attempting renewal..."
    certbot renew --quiet --cert-name "${DOMAIN}" || true
else
    certbot certonly \
        --nginx \
        -d "${DOMAIN}" \
        --non-interactive \
        --agree-tos \
        --email "admin@${DOMAIN}" \
        --no-eff-email
    ok "Certificates obtained"
fi

[[ -f "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem" ]] || die "Certificate not found after certbot"
ok "Certificates ready"

# ─── Step 8: Copy certs for XMPP (waddle-readable) ─────────────────────────
info "Step 8: Copying certificates for XMPP TLS..."

copy_certs() {
    cp "/etc/letsencrypt/live/${DOMAIN}/fullchain.pem" "${CERT_DIR}/fullchain.pem"
    cp "/etc/letsencrypt/live/${DOMAIN}/privkey.pem"   "${CERT_DIR}/privkey.pem"
    chown "${SERVICE_USER}:${SERVICE_USER}" "${CERT_DIR}/fullchain.pem" "${CERT_DIR}/privkey.pem"
    chmod 644 "${CERT_DIR}/fullchain.pem"
    chmod 600 "${CERT_DIR}/privkey.pem"
}
copy_certs
ok "Certificates copied to ${CERT_DIR}"

# ─── Step 9: Full nginx configuration ───────────────────────────────────────
info "Step 9: Installing production nginx configuration..."

NGINX_CONF="/etc/nginx/sites-available/${DOMAIN}"
cat > "${NGINX_CONF}" <<'NGINX_EOF'
# Redirect HTTP → HTTPS
server {
    listen 80;
    listen [::]:80;
    server_name DOMAIN_PLACEHOLDER;

    location /.well-known/acme-challenge/ {
        root /var/www/html;
    }

    location / {
        return 301 https://$host$request_uri;
    }
}

# HTTPS reverse proxy
server {
    LISTEN_443_PLACEHOLDER
    server_name DOMAIN_PLACEHOLDER;

    ssl_certificate     /etc/letsencrypt/live/DOMAIN_PLACEHOLDER/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/DOMAIN_PLACEHOLDER/privkey.pem;

    # TLS hardening
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384;
    ssl_prefer_server_ciphers off;
    ssl_session_timeout 1d;
    ssl_session_cache shared:SSL:10m;
    ssl_session_tickets off;

    # Security headers
    add_header Strict-Transport-Security "max-age=63072000; includeSubDomains; preload" always;
    add_header X-Frame-Options DENY always;
    add_header X-Content-Type-Options nosniff always;
    add_header Referrer-Policy strict-origin-when-cross-origin always;

    # XMPP over WebSocket (RFC 7395)
    location /xmpp-websocket {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    # All other traffic
    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 60s;
    }
}
NGINX_EOF

# Detect nginx version for http2 directive compatibility
NGINX_VER=$(nginx -v 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' | head -1)
NGINX_MAJOR=$(echo "${NGINX_VER}" | cut -d. -f1)
NGINX_MINOR=$(echo "${NGINX_VER}" | cut -d. -f2)
info "Detected nginx ${NGINX_VER}"

if (( NGINX_MAJOR > 1 )) || (( NGINX_MAJOR == 1 && NGINX_MINOR >= 25 )); then
    # nginx >= 1.25.1: use standalone http2 directive
    LISTEN_BLOCK="listen 443 ssl;\n    listen [::]:443 ssl;\n    http2 on;"
else
    # nginx < 1.25: use http2 on listen line
    LISTEN_BLOCK="listen 443 ssl http2;\n    listen [::]:443 ssl http2;"
fi

sed -i "s|LISTEN_443_PLACEHOLDER|${LISTEN_BLOCK}|g" "${NGINX_CONF}"

# Replace placeholder with actual domain
sed -i "s/DOMAIN_PLACEHOLDER/${DOMAIN}/g" "${NGINX_CONF}"

# Enable production config, remove bootstrap
rm -f "/etc/nginx/sites-enabled/${DOMAIN}-bootstrap"
ln -sf "${NGINX_CONF}" "/etc/nginx/sites-enabled/${DOMAIN}"

nginx -t || die "nginx configuration test failed"
systemctl reload nginx
ok "Production nginx configuration active"

# ─── Step 10: systemd service ───────────────────────────────────────────────
info "Step 10: Creating systemd service..."

# Preserve existing env values on re-runs, generate defaults for first deploy
read_existing() {
    local key="$1" default="$2"
    if [[ -f "${ENV_FILE}" ]] && grep -q "^${key}=" "${ENV_FILE}"; then
        grep "^${key}=" "${ENV_FILE}" | head -1 | cut -d= -f2-
    else
        echo "${default}"
    fi
}

ENV_SESSION_KEY=$(read_existing WADDLE_SESSION_KEY "$(openssl rand -hex 32)")
ENV_RUST_LOG=$(read_existing RUST_LOG "info")
ENV_REGISTRATION=$(read_existing WADDLE_REGISTRATION_ENABLED "false")
ENV_NATIVE_AUTH=$(read_existing WADDLE_NATIVE_AUTH_ENABLED "true")
ENV_S2S=$(read_existing WADDLE_XMPP_S2S_ENABLED "false")

if [[ -f "${ENV_FILE}" ]]; then
    info "Updating env file (preserving session key and operator-tuned settings)"
else
    info "Creating env file with generated session key"
fi

cat > "${ENV_FILE}" <<EOF
# Waddle Server Environment — managed by deploy.sh
# Values marked [preserved] are kept across re-deploys.
# To change, edit this file and run: systemctl restart waddle

# Mode
WADDLE_MODE=homeserver
WADDLE_BASE_URL=https://${DOMAIN}

# Database (file-based, NOT in-memory)
WADDLE_DB_PATH=${DATA_DIR}/waddle.db

# Uploads
WADDLE_UPLOAD_DIR=${DATA_DIR}/uploads

# XMPP
WADDLE_XMPP_ENABLED=true
WADDLE_XMPP_DOMAIN=${DOMAIN}
WADDLE_XMPP_PORT=5222
WADDLE_XMPP_TLS_CERT=${CERT_DIR}/fullchain.pem
WADDLE_XMPP_TLS_KEY=${CERT_DIR}/privkey.pem
WADDLE_XMPP_MAM_DB=${DATA_DIR}/mam.db
WADDLE_XMPP_S2S_ENABLED=${ENV_S2S}
WADDLE_NATIVE_AUTH_ENABLED=${ENV_NATIVE_AUTH}
WADDLE_REGISTRATION_ENABLED=${ENV_REGISTRATION}

# Security [preserved]
WADDLE_SESSION_KEY=${ENV_SESSION_KEY}

# Graceful restart (Ecdysis)
WADDLE_DRAIN_TIMEOUT_SECS=30

# Logging [preserved]
RUST_LOG=${ENV_RUST_LOG}
EOF

chmod 600 "${ENV_FILE}"
chown root:root "${ENV_FILE}"

cat > /etc/systemd/system/waddle.service <<EOF
[Unit]
Description=Waddle Social Server (XMPP + HTTP)
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
User=${SERVICE_USER}
Group=${SERVICE_USER}
WorkingDirectory=${DATA_DIR}
ExecStart=${INSTALL_DIR}/target/release/${BINARY_NAME}
EnvironmentFile=${ENV_FILE}

# Graceful restart (Ecdysis pattern)
# SIGQUIT triggers graceful restart (re-exec with fd passing)
# SIGTERM triggers graceful shutdown (drain and exit)
ExecReload=/bin/kill -QUIT \$MAINPID
KillSignal=SIGTERM
TimeoutStopSec=35

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=${DATA_DIR}
ReadOnlyPaths=${CERT_DIR}
PrivateTmp=true

# Restart policy
Restart=on-failure
RestartSec=5
StartLimitIntervalSec=60
StartLimitBurst=5

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=waddle

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
ok "systemd service created"

# ─── Step 11: Firewall ──────────────────────────────────────────────────────
info "Step 11: Configuring firewall..."

# Idempotent: add rules without resetting existing ones (avoids SSH lockout)
ufw default deny incoming > /dev/null 2>&1
ufw default allow outgoing > /dev/null 2>&1
ufw allow 22/tcp   > /dev/null 2>&1   # SSH (always first to prevent lockout)
ufw allow 80/tcp   > /dev/null 2>&1   # HTTP (certbot + redirect)
ufw allow 443/tcp  > /dev/null 2>&1   # HTTPS
ufw allow 5222/tcp > /dev/null 2>&1   # XMPP C2S
ufw --force enable > /dev/null 2>&1
ok "Firewall configured (22, 80, 443, 5222 open; 3000 blocked by default-deny)"

# ─── Step 12: Enable and start services ─────────────────────────────────────
info "Step 12: Starting services..."

# Stop waddle first if running (for re-deploys)
systemctl stop waddle 2>/dev/null || true

systemctl enable nginx --quiet
systemctl enable waddle --quiet
systemctl start waddle

# Wait for startup
sleep 3

if systemctl is-active --quiet waddle; then
    ok "Waddle service is running"
else
    warn "Service failed to start with new binary!"
    journalctl -u waddle --no-pager -n 20

    # Attempt rollback to previous binary
    PREV_BINARY="${INSTALL_DIR}/target/release/${BINARY_NAME}.prev"
    if [[ -f "${PREV_BINARY}" ]]; then
        warn "Rolling back to previous binary..."
        cp "${PREV_BINARY}" "${INSTALL_DIR}/target/release/${BINARY_NAME}"
        systemctl start waddle
        sleep 2
        if systemctl is-active --quiet waddle; then
            warn "Rollback successful — service running with previous version"
            warn "Investigate the new build and re-deploy when fixed"
            exit 1
        else
            die "Rollback also failed — manual intervention required"
        fi
    else
        die "Waddle service failed to start — no previous binary to rollback to"
    fi
fi

# ─── Step 13: Certbot renewal hook ──────────────────────────────────────────
info "Step 13: Installing certbot renewal hook..."

mkdir -p /etc/letsencrypt/renewal-hooks/deploy
cat > /etc/letsencrypt/renewal-hooks/deploy/waddle.sh <<HOOK
#!/usr/bin/env bash
# Auto-deployed by deploy.sh — copies renewed certs and restarts waddle
set -euo pipefail

DOMAIN="${DOMAIN}"
CERT_DIR="${CERT_DIR}"
SERVICE_USER="${SERVICE_USER}"

cp "/etc/letsencrypt/live/\${DOMAIN}/fullchain.pem" "\${CERT_DIR}/fullchain.pem"
cp "/etc/letsencrypt/live/\${DOMAIN}/privkey.pem"   "\${CERT_DIR}/privkey.pem"
chown "\${SERVICE_USER}:\${SERVICE_USER}" "\${CERT_DIR}/fullchain.pem" "\${CERT_DIR}/privkey.pem"
chmod 644 "\${CERT_DIR}/fullchain.pem"
chmod 600 "\${CERT_DIR}/privkey.pem"

systemctl restart waddle
logger "waddle: TLS certificates renewed and service restarted"
HOOK

chmod 755 /etc/letsencrypt/renewal-hooks/deploy/waddle.sh
ok "Certbot renewal hook installed"

# ─── Step 14: Smoke tests ───────────────────────────────────────────────────
info "Step 14: Running post-deploy smoke tests..."

TESTS_PASSED=0
TESTS_TOTAL=0

run_test() {
    local name="$1"; shift
    TESTS_TOTAL=$((TESTS_TOTAL + 1))
    if "$@" > /dev/null 2>&1; then
        ok "  ${name}"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        warn "  ${name} — FAILED"
    fi
}

run_test "HTTP health (local)" \
    curl -sf --max-time 5 http://127.0.0.1:3000/health

run_test "HTTP health (via nginx/TLS)" \
    curl -sf --max-time 5 "https://${DOMAIN}/health"

run_test "Server info endpoint" \
    curl -sf --max-time 5 "https://${DOMAIN}/api/v1/server-info"

# XMPP STARTTLS needs a pipeline, so wrap in a function
test_xmpp_starttls() {
    echo 'QUIT' | openssl s_client -connect "${DOMAIN}:5222" -starttls xmpp -servername "${DOMAIN}" 2>&1 | grep -q 'Certificate chain\|CONNECTED'
}
run_test "XMPP STARTTLS on port 5222" test_xmpp_starttls

run_test "systemd service active" \
    systemctl is-active --quiet waddle

run_test "systemd service enabled" \
    systemctl is-enabled --quiet waddle

echo ""
if [[ ${TESTS_PASSED} -eq ${TESTS_TOTAL} ]]; then
    ok "All ${TESTS_TOTAL}/${TESTS_TOTAL} smoke tests passed"
else
    warn "${TESTS_PASSED}/${TESTS_TOTAL} smoke tests passed"
    warn "Review failures above — the service is running but some checks failed"
    warn "Deploy completed with warnings — exiting non-zero"
    # Print the summary banner before exiting so the operator sees connection details
    echo ""
    echo "════════════════════════════════════════════════════════════════"
    warn "Waddle deployed to ${DOMAIN} (with smoke test failures)"
    echo ""
    echo "  Logs:   journalctl -u waddle -f"
    echo "  Status: systemctl status waddle"
    echo "  Config: ${ENV_FILE}"
    echo "════════════════════════════════════════════════════════════════"
    exit 1
fi

# ─── Done ────────────────────────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════════════════════"
ok "Waddle deployed to ${DOMAIN}"
echo ""
echo "  Web:    https://${DOMAIN}"
echo "  XMPP:   ${DOMAIN}:5222 (STARTTLS)"
echo "  WS:     wss://${DOMAIN}/xmpp-websocket"
echo ""
echo "  Logs:   journalctl -u waddle -f"
echo "  Status: systemctl status waddle"
echo "  Config: ${ENV_FILE}"
echo "  Data:   ${DATA_DIR}/"
echo ""
echo "  Remaining manual steps:"
echo "    1. Add DNS SRV record: _xmpp-client._tcp.${DOMAIN} → ${DOMAIN}:5222"
echo "    2. Set up DB backups:  cron + sqlite3 ${DATA_DIR}/waddle.db '.backup ...'"
echo "════════════════════════════════════════════════════════════════"
