# Teleport Setup and Access Guide

This guide covers the complete setup of Teleport for secure access to Proxmox and Kubernetes infrastructure.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Initial Deployment](#initial-deployment)
3. [DNS and Firewall Configuration](#dns-and-firewall-configuration)
4. [Teleport Installation](#teleport-installation)
5. [First-Time Setup](#first-time-setup)
6. [Proxmox Integration](#proxmox-integration)
7. [Kubernetes Integration](#kubernetes-integration)
8. [User Management](#user-management)
9. [Team Access Procedures](#team-access-procedures)
10. [Security Best Practices](#security-best-practices)
11. [Troubleshooting](#troubleshooting)

## Architecture Overview

**Teleport Components:**
- **Auth Service**: Authentication, authorization, and audit logging
- **Proxy Service**: Public-facing gateway (HTTPS/SSH)
- **Agents**: Connect infrastructure resources via reverse tunnels

**Deployment:**
- Auth + Proxy: Dedicated VM on Proxmox (single public IP)
- Kube Agent: Deployed in Kubernetes cluster (after Cilium)
- Proxmox Access: Configured via Application Access and SSH Access

**Security Model:**
- Single public IP exposes only Teleport Proxy (ports 443, 3024)
- All other services accessed through Teleport (reverse tunnels)
- Zero-trust access with MFA, RBAC, and session recording
- Comprehensive audit logging for compliance

## Initial Deployment

### 1. Configure Environment Variables

Edit `infrastructure/.env` and set Teleport configuration:

```bash
# Enable Teleport
TELEPORT_ENABLED=true

# Public domain (must point to your public IP)
TELEPORT_DOMAIN=teleport.waddle.social

# Let's Encrypt email
TELEPORT_LETSENCRYPT_EMAIL=admin@waddle.social

# Static IP for Teleport VM
TELEPORT_IP_ADDRESS=192.168.1.100
TELEPORT_GATEWAY=192.168.1.1

# SSH keys for initial access (comma-separated)
TELEPORT_SSH_KEYS="ssh-ed25519 AAAA..."
```

### 2. Deploy Teleport VM

```bash
cd infrastructure
npm run synth
npm run deploy
```

This provisions a Teleport VM on Proxmox with:
- Debian 12 base image
- Static IP and SSH key access
- QEMU guest agent enabled

### 3. Verify Deployment

Check Terraform outputs:

```bash
cdktf output
```

Expected outputs:
- `teleport_vm_id`: Proxmox VM ID
- `teleport_ip`: VM IP address
- `teleport_domain`: Public domain
- `teleport_web_ui`: Web UI URL

## DNS and Firewall Configuration

### DNS Records

Create an A record pointing to your public IP:

```
teleport.waddle.social.  A  <your-public-ip>
```

Verify DNS propagation:

```bash
dig teleport.waddle.social +short
```

### Firewall Port Forwarding

Configure your router/firewall to forward:

| External Port | Internal IP | Internal Port | Protocol | Purpose |
|---------------|-------------|---------------|----------|----------|
| 443 | 192.168.1.100 | 443 | TCP | Web UI / Proxy |
| 3024 | 192.168.1.100 | 3024 | TCP | SSH Proxy |

**Note:** Port 3025 (reverse tunnel) is outbound-only, no forwarding needed.

### Verify Connectivity

After Teleport is installed, test external access:

```bash
curl -I https://teleport.waddle.social
```

Expected: HTTP 200 or redirect to login page.

## Teleport Installation

After the VM is provisioned, SSH in and install Teleport:

### 1. SSH to Teleport VM

Use the SSH key configured in `TELEPORT_SSH_KEYS`:

```bash
ssh admin@192.168.1.100
```

### 2. Install Teleport

Run the following commands to install Teleport from the official repository:

```bash
# Add Teleport APT repository
curl https://apt.releases.teleport.dev/gpg -o /usr/share/keyrings/teleport-archive-keyring.asc
echo "deb [signed-by=/usr/share/keyrings/teleport-archive-keyring.asc] https://apt.releases.teleport.dev/debian bookworm stable/v17" | sudo tee /etc/apt/sources.list.d/teleport.list

# Install Teleport
sudo apt-get update
sudo apt-get install -y teleport
```

### 3. Configure Teleport

Create the Teleport configuration file:

```bash
sudo tee /etc/teleport.yaml > /dev/null <<EOF
version: v3
teleport:
  nodename: teleport
  data_dir: /var/lib/teleport
  log:
    output: stderr
    severity: INFO

auth_service:
  enabled: true
  listen_addr: 0.0.0.0:3025
  cluster_name: teleport.waddle.social  # Replace with your domain
  authentication:
    type: local
    second_factor: "on"
    webauthn:
      rp_id: teleport.waddle.social     # Replace with your domain
  session_recording: node

proxy_service:
  enabled: true
  web_listen_addr: 0.0.0.0:443
  public_addr: teleport.waddle.social:443     # Replace with your domain
  tunnel_listen_addr: 0.0.0.0:3024
  tunnel_public_addr: teleport.waddle.social:3024  # Replace with your domain
  acme:
    enabled: true
    email: admin@waddle.social  # Replace with your email

ssh_service:
  enabled: true
  labels:
    env: production
    role: teleport-server
EOF
```

### 4. Configure Firewall

```bash
sudo apt-get install -y ufw
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp comment 'SSH'
sudo ufw allow 443/tcp comment 'Teleport Web UI and Proxy'
sudo ufw allow 3024/tcp comment 'Teleport SSH Proxy'
sudo ufw allow 3025/tcp comment 'Teleport Auth Service'
sudo ufw --force enable
```

### 5. Start Teleport

```bash
sudo systemctl enable teleport
sudo systemctl start teleport
```

### 6. Verify Installation

```bash
sudo systemctl status teleport
sudo journalctl -u teleport -f
```

## First-Time Setup

### 1. Create First Admin User

Generate a signup token:

```bash
sudo tctl users add admin --roles=editor,access --logins=root,admin
```

Output:
```
User "admin" has been created but requires a password. Share this URL with the user to complete user setup, link is valid for 1h:
https://teleport.waddle.social:443/web/invite/<token>

NOTE: Make sure teleport.waddle.social:443 points at a Teleport proxy which users can access.
```

### 2. Complete User Setup

1. Open the invite URL in a browser
2. Set a strong password
3. Configure MFA (authenticator app or hardware key)
4. Login to Teleport web UI

### 3. Verify Teleport Status

Check service status:

```bash
sudo systemctl status teleport
sudo journalctl -u teleport -f
```

## Proxmox Integration

### Application Access (Proxmox Web UI)

#### 1. Create Application Resource

On the Teleport VM, create the application configuration:

```bash
sudo tee /etc/teleport.d/proxmox-app.yaml > /dev/null <<EOF
kind: app
version: v3
metadata:
  name: proxmox-web
spec:
  uri: https://192.168.1.1:8006
  public_addr: proxmox.teleport.waddle.social
  labels:
    env: production
    type: proxmox
  insecure_skip_verify: true  # If using self-signed cert
EOF
```

#### 2. Apply Configuration

```bash
sudo tctl create -f /etc/teleport.d/proxmox-app.yaml
```

#### 3. Access Proxmox Web UI

Users access Proxmox through Teleport:

```bash
# Login to Teleport
tsh login --proxy=teleport.waddle.social:443 --user=admin

# List available apps
tsh apps ls

# Login to Proxmox app
tsh apps login proxmox-web

# Open in browser
tsh apps open proxmox-web
```

Or access directly via: `https://proxmox.teleport.waddle.social`

### SSH Access (Proxmox Host)

#### 1. Install Teleport SSH Agent on Proxmox Host

On the Proxmox host:

```bash
# Add Teleport APT repository
curl https://apt.releases.teleport.dev/gpg | sudo apt-key add -
echo "deb https://apt.releases.teleport.dev/ stable main" | sudo tee /etc/apt/sources.list.d/teleport.list
sudo apt update
sudo apt install teleport
```

#### 2. Generate Join Token

On the Teleport VM:

```bash
sudo tctl tokens add --type=node --ttl=1h
```

Copy the generated token.

#### 3. Configure Teleport Agent

On the Proxmox host, create `/etc/teleport.yaml`:

```yaml
teleport:
  auth_token: <token-from-step-2>
  proxy_server: teleport.waddle.social:443

auth_service:
  enabled: false

proxy_service:
  enabled: false

ssh_service:
  enabled: true
  labels:
    env: production
    role: proxmox-host
    hostname: pve
```

#### 4. Start Teleport Agent

```bash
sudo systemctl enable teleport
sudo systemctl start teleport
```

#### 5. Access Proxmox via SSH

Users access Proxmox SSH through Teleport:

```bash
# Login to Teleport
tsh login --proxy=teleport.waddle.social:443 --user=admin

# List available SSH nodes
tsh ls

# SSH to Proxmox host
tsh ssh root@pve
```

## Kubernetes Integration

**Note:** Kubernetes integration is configured after Cilium CNI is installed (Phase 6).

See `infrastructure-k8s/teleport/README.md` for detailed Kubernetes integration steps.

**Summary:**
1. Generate join token for Kube Agent
2. Deploy Teleport Kube Agent via Helm
3. Configure Kubernetes RBAC roles
4. Access cluster via `tsh kube login`

## User Management

### Creating Users

#### Via CLI (on Teleport VM)

```bash
# Create user with specific roles
sudo tctl users add alice --roles=developer,access --logins=alice

# Create admin user
sudo tctl users add bob --roles=editor,access --logins=root,admin,bob
```

#### Via Web UI

1. Login to Teleport web UI as admin
2. Navigate to **Users** → **Add User**
3. Fill in username, roles, and allowed logins
4. Send invite link to user

### User Roles

**Built-in Roles:**
- `editor`: Full admin access (create/edit/delete resources)
- `access`: Standard user access
- `auditor`: Read-only access to audit logs

**Custom Roles:**

Create custom roles for Kubernetes access:

```bash
sudo tctl create -f /path/to/role.yaml
```

See `infrastructure-k8s/teleport/roles/` for examples.

### Assigning Roles

```bash
# Add role to existing user
sudo tctl users update alice --set-roles=developer,kubernetes-developer

# Remove role
sudo tctl users update alice --set-roles=developer
```

## Team Access Procedures

### For Team Members

#### 1. Install Teleport Client

**macOS:**
```bash
brew install teleport
```

**Linux:**
```bash
curl https://get.gravitational.com/teleport-v17.0.0-linux-amd64-bin.tar.gz | tar -xz
sudo ./teleport/install
```

**Windows:**
Download from: https://goteleport.com/download/

#### 2. Initial Login

Receive invite link from admin, complete setup:

1. Open invite URL
2. Set password
3. Configure MFA (required)

#### 3. Login via CLI

```bash
tsh login --proxy=teleport.waddle.social:443 --user=<your-username>
```

Enter password and MFA code.

#### 4. Access Resources

**List available resources:**
```bash
tsh ls                    # SSH nodes
tsh apps ls               # Applications
tsh kube ls               # Kubernetes clusters
```

**SSH to Proxmox:**
```bash
tsh ssh root@pve
```

**Access Proxmox Web UI:**
```bash
tsh apps login proxmox-web
tsh apps open proxmox-web
```

**Access Kubernetes:**
```bash
tsh kube login waddle-cluster
kubectl get nodes
```

#### 5. Session Management

**Check active sessions:**
```bash
tsh status
```

**Logout:**
```bash
tsh logout
```

### For Administrators

#### Audit Logs

View audit logs:

```bash
# On Teleport VM
sudo journalctl -u teleport -f

# Query audit events
sudo tctl get events --format=json
```

#### Session Recordings

View recorded sessions in web UI:
1. Login to Teleport web UI
2. Navigate to **Activity** → **Session Recordings**
3. Filter by user, date, or resource

## Security Best Practices

### 1. Multi-Factor Authentication

**Require MFA for all users:**

Edit `/etc/teleport.yaml` on Teleport VM:

```yaml
auth_service:
  authentication:
    type: local
    second_factor: on
    require_session_mfa: true
```

**Supported MFA methods:**
- TOTP (Google Authenticator, Authy)
- WebAuthn (YubiKey, Touch ID)
- Hardware security keys (recommended for production)

### 2. Session Recording

Enable session recording for compliance:

```yaml
auth_service:
  session_recording: node
```

Options:
- `node`: Record at the node level
- `proxy`: Record at the proxy level
- `off`: Disable recording (not recommended)

### 3. Role-Based Access Control

**Principle of Least Privilege:**
- Grant minimum necessary permissions
- Use custom roles for specific access patterns
- Regularly review and audit role assignments

### 4. Access Requests

Implement just-in-time access for production:

```yaml
kind: role
version: v7
metadata:
  name: developer
spec:
  allow:
    request:
      roles: ["production-access"]
      thresholds:
        - approve: 1
          deny: 1
```

Users request temporary elevated access:

```bash
tsh request create --roles=production-access --reason="Deploy hotfix"
```

Admins approve/deny via web UI or CLI.

### 5. Certificate Rotation

Rotate certificates regularly:

```bash
# Rotate CA (requires cluster restart)
sudo tctl auth rotate --type=host
```

## Troubleshooting

### Cannot Access Teleport Web UI

**Check DNS:**
```bash
dig teleport.waddle.social +short
```

**Check firewall:**
```bash
# From external network
telnet teleport.waddle.social 443
```

**Check Teleport service:**
```bash
ssh admin@192.168.1.100
sudo systemctl status teleport
sudo journalctl -u teleport -n 100
```

### Certificate Errors

**Let's Encrypt rate limits:**
- Use staging environment for testing
- Check rate limits: https://letsencrypt.org/docs/rate-limits/

**Manual certificate:**
If Let's Encrypt fails, use manual certificate:

```yaml
proxy_service:
  https_keypairs:
    - key_file: /etc/teleport/tls.key
      cert_file: /etc/teleport/tls.crt
```

### SSH Access Denied

**Check user roles:**
```bash
sudo tctl get users/<username>
```

**Check node labels:**
```bash
sudo tctl get nodes
```

**Verify role allows access:**
```bash
sudo tctl get roles/<role-name>
```

### Kubernetes Access Issues

**Agent not connecting:**
```bash
kubectl logs -n teleport -l app=teleport-kube-agent
```

**Check join token:**
```bash
sudo tctl tokens ls
```

**Verify network connectivity:**
```bash
# From Kubernetes cluster
curl -I https://teleport.waddle.social
```

### Performance Issues

**Check resource usage:**
```bash
ssh admin@192.168.1.100
top
df -h
```

**Increase VM resources:**
Edit `TELEPORT_CORES` and `TELEPORT_MEMORY` in `.env`, redeploy.

**Enable debug logging:**
```yaml
teleport:
  log:
    severity: DEBUG
```

## References

- [Teleport Documentation](https://goteleport.com/docs/)
- [Teleport Proxmox Integration](https://goteleport.com/integrations/proxmox/)
- [Teleport Kubernetes Access](https://goteleport.com/docs/kubernetes-access/)
- [Teleport RBAC](https://goteleport.com/docs/access-controls/reference/)
- [Teleport Security Best Practices](https://goteleport.com/docs/access-controls/guides/best-practices/)
