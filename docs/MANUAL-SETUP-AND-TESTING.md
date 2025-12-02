# Manual Setup Steps and Cluster Testing Guide

This document outlines all manual steps required before deploying the cluster and provides a step-by-step testing procedure to verify each component.

## Table of Contents

- [Part 1: Manual Prerequisites](#part-1-manual-prerequisites)
  - [1.1 Proxmox Configuration](#11-proxmox-configuration)
  - [1.2 Network and DNS Setup](#12-network-and-dns-setup)
  - [1.3 Secrets and Credentials](#13-secrets-and-credentials)
- [Part 2: Step-by-Step Cluster Deployment and Testing](#part-2-step-by-step-cluster-deployment-and-testing)
  - [Phase 1-4: Infrastructure Provisioning](#phase-1-4-infrastructure-provisioning)
  - [Phase 5: Teleport Setup](#phase-5-teleport-setup)
  - [Phase 6: Cilium CNI](#phase-6-cilium-cni)
  - [Phase 7: Proxmox CSI Storage](#phase-7-proxmox-csi-storage)
  - [Phase 8: Flux GitOps](#phase-8-flux-gitops)
  - [Phase 9: cert-manager](#phase-9-cert-manager)
  - [Phase 10: Gateway API](#phase-10-gateway-api)
  - [Phase 11: CloudNativePG](#phase-11-cloudnativepg)
  - [Phase 12: SpiceDB](#phase-12-spicedb)
  - [Phase 13: Observability Stack](#phase-13-observability-stack)
  - [Phase 14-15: Network Policies and Alerting](#phase-14-15-network-policies-and-alerting)

---

## Part 1: Manual Prerequisites

These steps MUST be completed manually before deploying the cluster.

### 1.1 Proxmox Configuration

#### 1.1.1 Create Proxmox API Token for Terraform

```bash
# SSH to Proxmox host
ssh root@<proxmox-host>

# Create API token for Terraform/CDKTF
pveum user token add root@pam terraform -privsep 0

# SAVE THE OUTPUT - Token secret is shown only once!
# Format: root@pam!terraform=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
```

#### 1.1.2 Create Proxmox CSI User and Token

```bash
# Still on Proxmox host

# Create CSI role with required permissions
pveum role add CSI -privs "VM.Audit VM.Config.Disk Datastore.Allocate Datastore.AllocateSpace Datastore.Audit"

# Create dedicated user
pveum user add kubernetes-csi@pve

# Assign role to user
pveum aclmod / -user kubernetes-csi@pve -role CSI

# Create API token (no privilege separation)
pveum user token add kubernetes-csi@pve csi -privsep 0

# SAVE THE OUTPUT - Token secret is shown only once!
# Format: kubernetes-csi@pve!csi=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
```

#### 1.1.3 Verify Proxmox Storage

```bash
# On Proxmox host
pvesm status

# Note which storage supports 'iso' content (for Talos images)
# Note which storage to use for VM disks (local-lvm, local-zfs, ceph-pool, etc.)
```

### 1.2 Network and DNS Setup

#### 1.2.1 IP Address Planning

Reserve the following IP addresses in your network:

| Purpose | Example IP | Notes |
|---------|------------|-------|
| Control Plane 1 | 192.168.1.101 | First Talos CP node |
| Control Plane 2 | 192.168.1.102 | Second Talos CP node |
| Control Plane 3 | 192.168.1.103 | Third Talos CP node |
| Worker nodes (if any) | 192.168.1.104+ | Sequential after CPs |
| Teleport VM | 192.168.1.100 | Secure access gateway |
| Gateway LoadBalancer | (assigned by Cilium) | For ingress traffic |

#### 1.2.2 DNS Records (Cloudflare)

Create these DNS records BEFORE deploying:

| Type | Name | Value | Proxy |
|------|------|-------|-------|
| A | teleport.waddle.social | Your public IP | DNS only (gray cloud) |
| A | waddle.social | Gateway LB IP* | DNS only |
| A | *.waddle.social | Gateway LB IP* | DNS only |

*Gateway LB IP is obtained after Phase 10 deployment - update DNS then.

#### 1.2.3 Firewall/Router Port Forwarding

Configure port forwarding on your router:

| External Port | Internal IP | Internal Port | Purpose |
|---------------|-------------|---------------|---------|
| 443 | Teleport VM IP | 443 | Teleport Web UI |
| 3024 | Teleport VM IP | 3024 | Teleport SSH Proxy |
| 80 | Gateway LB IP* | 80 | HTTP (redirect to HTTPS) |
| 443 | Gateway LB IP* | 443 | HTTPS ingress |

*Configure after obtaining Gateway LB IP in Phase 10.

### 1.3 Secrets and Credentials

#### 1.3.1 Create Cloudflare API Token

1. Go to https://dash.cloudflare.com/profile/api-tokens
2. Click "Create Token"
3. Use "Edit zone DNS" template
4. Configure:
   - Permissions: Zone > DNS > Edit
   - Zone Resources: Include > All zones (or specific zones)
5. Create and SAVE the token

#### 1.3.2 Environment File Setup

```bash
cd infrastructure
cp .env.example .env

# Edit .env with your values:
# - PROXMOX_VE_ENDPOINT
# - PROXMOX_VE_API_TOKEN (from step 1.1.1)
# - PROXMOX_NODE_NAME
# - TALOS_* settings
# - TELEPORT_* settings (if using Teleport)
```

#### 1.3.3 Kubernetes Secrets to Create (After Cluster Bootstrap)

These secrets must be created manually in the cluster:

**1. Cloudflare API Token (for cert-manager)**
```bash
kubectl create namespace cert-manager
kubectl create secret generic cloudflare-api-token \
  --from-literal=api-token=<your-cloudflare-token> \
  -n cert-manager
```

**2. Proxmox CSI Credentials**
```bash
kubectl create namespace csi-proxmox

# Create config file
cat > /tmp/proxmox-csi-config.yaml << EOF
clusters:
  - url: "https://<proxmox-host>:8006/api2/json"
    insecure: false
    token_id: "kubernetes-csi@pve!csi"
    token_secret: "<token-secret-from-step-1.1.2>"
    region: "proxmox"
EOF

kubectl create secret generic proxmox-csi-credentials \
  --from-file=config.yaml=/tmp/proxmox-csi-config.yaml \
  -n csi-proxmox

rm /tmp/proxmox-csi-config.yaml
```

**3. Grafana Admin Password**
```bash
kubectl create namespace observability
kubectl create secret generic grafana-admin \
  --from-literal=admin-user=admin \
  --from-literal=admin-password=$(openssl rand -base64 32) \
  -n observability

# SAVE the password for later access
kubectl get secret grafana-admin -n observability -o jsonpath='{.data.admin-password}' | base64 -d && echo
```

**4. Alertmanager Configuration**
```bash
# Create alertmanager config file
cat > /tmp/alertmanager.yaml << 'EOF'
global:
  resolve_timeout: 5m

route:
  group_by: ['alertname', 'namespace', 'severity']
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h
  receiver: 'default-receiver'
  routes:
    - match:
        severity: critical
      receiver: 'critical-receiver'
      repeat_interval: 1h

receivers:
  - name: 'default-receiver'
    # Configure your notification channel here
    # slack_configs:
    #   - api_url: 'https://hooks.slack.com/services/xxx'
    #     channel: '#alerts'

  - name: 'critical-receiver'
    # Configure critical alert notifications
    # slack_configs:
    #   - api_url: 'https://hooks.slack.com/services/xxx'
    #     channel: '#critical-alerts'

inhibit_rules:
  - source_match:
      severity: 'critical'
    target_match:
      severity: 'warning'
    equal: ['alertname', 'namespace']
EOF

kubectl create secret generic alertmanager-config \
  --from-file=alertmanager.yaml=/tmp/alertmanager.yaml \
  -n observability

rm /tmp/alertmanager.yaml
```

**5. SpiceDB Preshared Key**
```bash
kubectl create namespace spicedb
kubectl create secret generic spicedb-config \
  --from-literal=preshared_key=$(openssl rand -hex 32) \
  -n spicedb

# SAVE the key for API access
kubectl get secret spicedb-config -n spicedb -o jsonpath='{.data.preshared_key}' | base64 -d && echo
```

---

## Part 2: Step-by-Step Cluster Deployment and Testing

### Phase 1-4: Infrastructure Provisioning

#### Deploy Infrastructure

```bash
cd infrastructure
npm install
npm run get      # Generate provider bindings (first time only)
npm run synth    # Generate Terraform config
npm run deploy   # Provision VMs and bootstrap cluster
```

#### Test Phase 1-4

```bash
# 1. Verify VMs in Proxmox UI
# Check: All VMs created and running

# 2. Extract kubeconfig
cdktf output -raw kubeconfig_raw | base64 -d > ~/.kube/config

# 3. Extract talosconfig
mkdir -p ~/.talos
cdktf output -raw talosconfig_raw | base64 -d > ~/.talos/config
export TALOSCONFIG=~/.talos/config

# 4. Test Kubernetes API access
kubectl get nodes
# Expected: All nodes in Ready status (may take a few minutes)

# 5. Test Talos API access
talosctl -n <first-cp-ip> version
talosctl -n <first-cp-ip> health

# 6. Verify etcd health
talosctl -n <first-cp-ip> etcd members
# Expected: 3 members for HA cluster

# 7. Check cluster info
kubectl cluster-info
```

**Expected Output:**
- All nodes show `STATUS: Ready`
- etcd has 3 members (for 3 CP nodes)
- Kubernetes API responds

---

### Phase 5: Teleport Setup

Skip if `TELEPORT_ENABLED=false`.

#### Test Phase 5

```bash
# 1. SSH to Teleport VM
ssh admin@<TELEPORT_IP_ADDRESS>

# 2. Install Teleport (on Teleport VM)
curl https://apt.releases.teleport.dev/gpg -o /usr/share/keyrings/teleport-archive-keyring.asc
echo "deb [signed-by=/usr/share/keyrings/teleport-archive-keyring.asc] https://apt.releases.teleport.dev/debian bookworm stable/v17" | sudo tee /etc/apt/sources.list.d/teleport.list
sudo apt-get update && sudo apt-get install -y teleport

# 3. Configure and start Teleport
# Follow docs/teleport-setup.md

# 4. Test Teleport access
tsh login --proxy=teleport.waddle.social:443 --user=admin
tsh status
```

---

### Phase 6: Cilium CNI

Cilium is deployed automatically after Flux bootstrap. For manual verification before Flux:

#### Test Phase 6

```bash
# 1. Check Cilium pods
kubectl get pods -n kube-system -l k8s-app=cilium
# Expected: One pod per node, all Running

# 2. Check Cilium status (if cilium CLI installed)
cilium status --wait

# 3. Verify network connectivity
kubectl create deployment nginx --image=nginx
kubectl expose deployment nginx --port=80
kubectl run curl --image=curlimages/curl --rm -it --restart=Never -- curl nginx
# Expected: HTML response from nginx

# Cleanup
kubectl delete deployment nginx
kubectl delete svc nginx

# 4. Check Gateway API CRDs
kubectl get crds | grep gateway
# Expected: gateways.gateway.networking.k8s.io, httproutes.gateway.networking.k8s.io, etc.
```

---

### Phase 7: Proxmox CSI Storage

#### Prerequisites
- Proxmox CSI credentials secret created (see 1.3.3)

#### Test Phase 7

```bash
# 1. Check CSI driver pods
kubectl get pods -n csi-proxmox
# Expected: Controller and node plugins running

# 2. Verify StorageClass
kubectl get storageclass proxmox-csi
# Expected: StorageClass exists with PROVISIONER: csi.proxmox.sinextra.dev

# 3. Test PVC provisioning
cat <<EOF | kubectl apply -f -
apiVersion: v1
kind: Namespace
metadata:
  name: csi-test
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: test-pvc
  namespace: csi-test
spec:
  accessModes:
    - ReadWriteOnce
  storageClassName: proxmox-csi
  resources:
    requests:
      storage: 1Gi
EOF

# Wait for PVC to bind
kubectl get pvc -n csi-test -w
# Expected: STATUS changes from Pending to Bound

# 4. Test mounting the PVC
cat <<EOF | kubectl apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: test-pod
  namespace: csi-test
spec:
  containers:
  - name: test
    image: busybox
    command: ['sh', '-c', 'echo "CSI test successful" > /data/test.txt && cat /data/test.txt && sleep 3600']
    volumeMounts:
    - name: data
      mountPath: /data
  volumes:
  - name: data
    persistentVolumeClaim:
      claimName: test-pvc
EOF

# Check pod logs
kubectl logs -n csi-test test-pod
# Expected: "CSI test successful"

# Cleanup
kubectl delete namespace csi-test
```

---

### Phase 8: Flux GitOps

#### Prerequisites
- Git repository initialized and pushed to GitHub
- GitHub Personal Access Token

#### Deploy Flux

```bash
# 1. Initialize Git repo (if not done)
git init
git add .
git commit -m "Initial commit"
git remote add origin <your-repo-url>
git push -u origin main

# 2. Bootstrap Flux
export GITHUB_TOKEN=<your-github-pat>
flux bootstrap github \
  --owner=<github-org-or-username> \
  --repository=waddle-infra \
  --path=clusters/production \
  --personal
```

#### Test Phase 8

```bash
# 1. Check Flux installation
flux check
# Expected: All checks pass

# 2. Verify GitRepository source
flux get sources git
# Expected: flux-system ready

# 3. Check Kustomizations
flux get kustomizations
# Expected: flux-system and infrastructure kustomizations

# 4. Check HelmReleases
flux get helmreleases -A
# Expected: Various releases (may be progressing)

# 5. Force reconciliation if needed
flux reconcile kustomization flux-system --with-source
```

---

### Phase 9: cert-manager

#### Prerequisites
- Cloudflare API token secret created (see 1.3.3)

#### Test Phase 9

```bash
# 1. Check cert-manager pods
kubectl get pods -n cert-manager
# Expected: 3 pods (controller, webhook, cainjector) all Running

# 2. Verify ClusterIssuers
kubectl get clusterissuer
# Expected: letsencrypt-staging and letsencrypt-production

# 3. Test certificate issuance (staging first)
cat <<EOF | kubectl apply -f -
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: test-cert
  namespace: cert-manager
spec:
  secretName: test-cert-tls
  issuerRef:
    name: letsencrypt-staging
    kind: ClusterIssuer
  dnsNames:
    - test.waddle.social
EOF

# Watch certificate status
kubectl get certificate -n cert-manager -w
# Expected: READY becomes True (may take 1-2 minutes)

# 4. Check for challenges
kubectl get challenges -A
# If stuck, describe the challenge for errors

# 5. Verify certificate secret
kubectl get secret test-cert-tls -n cert-manager
# Expected: Secret exists with tls.crt and tls.key

# Cleanup
kubectl delete certificate test-cert -n cert-manager
kubectl delete secret test-cert-tls -n cert-manager
```

---

### Phase 10: Gateway API

#### Test Phase 10

```bash
# 1. Check Gateway resource
kubectl get gateway -n gateway-ingress
# Expected: gateway with PROGRAMMED=True

# 2. Get LoadBalancer IP
export GATEWAY_IP=$(kubectl get gateway gateway -n gateway-ingress -o jsonpath='{.status.addresses[0].value}')
echo "Gateway IP: $GATEWAY_IP"

# UPDATE DNS RECORDS NOW with this IP!

# 3. Check Gateway certificate
kubectl get certificate -n gateway-ingress
# Expected: gateway-tls with READY=True

# 4. Verify HTTPRoutes
kubectl get httproute -A

# 5. Test external access (after DNS propagates)
curl -v https://waddle.social
# Expected: TLS connection successful (may 404 if no routes configured)

# 6. Test with IP directly
curl -k -H "Host: waddle.social" https://$GATEWAY_IP/
```

---

### Phase 11: CloudNativePG

#### Test Phase 11

```bash
# 1. Check operator
kubectl get pods -n cnpg-system
# Expected: cloudnative-pg controller running

# 2. Verify CRDs
kubectl get crds | grep cnpg
# Expected: clusters.postgresql.cnpg.io

# 3. Deploy test cluster
kubectl apply -f infrastructure-k8s/cnpg/verification/namespace.yaml
kubectl apply -f infrastructure-k8s/cnpg/verification/sample-cluster.yaml

# 4. Watch cluster creation
kubectl get cluster -n cnpg-test -w
# Expected: STATUS becomes "Cluster in healthy state"

# 5. Check pods
kubectl get pods -n cnpg-test
# Expected: 3 pods (1 primary + 2 replicas)

# 6. Get credentials and connect
export PGPASSWORD=$(kubectl get secret sample-pg-cluster-superuser -n cnpg-test -o jsonpath='{.data.password}' | base64 -d)
kubectl port-forward -n cnpg-test sample-pg-cluster-1 5432:5432 &

# Test connection
psql -h localhost -U postgres -d testdb -c "SELECT version();"
# Expected: PostgreSQL version info

# Cleanup
kubectl delete namespace cnpg-test
```

---

### Phase 12: SpiceDB

#### Prerequisites
- SpiceDB preshared key secret created (see 1.3.3)
- CloudNativePG operator running (Phase 11)

#### Test Phase 12

```bash
# 1. Check SpiceDB operator
kubectl get pods -n spicedb -l app.kubernetes.io/name=spicedb-operator
# Expected: Operator running

# 2. Check SpiceDB pods
kubectl get pods -n spicedb -l app.kubernetes.io/name=spicedb
# Expected: 3 SpiceDB pods running

# 3. Check PostgreSQL cluster
kubectl get cluster -n spicedb
# Expected: Healthy PostgreSQL cluster

# 4. Test SpiceDB API
kubectl port-forward -n spicedb svc/spicedb 50051:50051 &

# Get preshared key
export SPICEDB_KEY=$(kubectl get secret spicedb-config -n spicedb -o jsonpath='{.data.preshared_key}' | base64 -d)

# Configure zed CLI (if installed)
zed context set local localhost:50051 "$SPICEDB_KEY" --insecure

# Test schema read
zed schema read
# Expected: Empty schema or existing schema
```

---

### Phase 13: Observability Stack

#### Prerequisites
- Grafana admin secret created (see 1.3.3)

#### Test Phase 13

```bash
# 1. Check all observability pods
kubectl get pods -n observability
# Expected: All pods Running (grafana, loki, tempo, mimir, otel-collector, prometheus-operator)

# 2. Check HelmReleases
flux get helmreleases -n observability
# Expected: All releases Ready

# 3. Check PVCs
kubectl get pvc -n observability
# Expected: PVCs bound for loki, tempo, mimir, grafana

# 4. Access Grafana
kubectl port-forward -n observability svc/grafana 3000:80 &

# Get password
export GRAFANA_PASS=$(kubectl get secret grafana-admin -n observability -o jsonpath='{.data.admin-password}' | base64 -d)
echo "Grafana password: $GRAFANA_PASS"

# Open http://localhost:3000, login with admin/$GRAFANA_PASS

# 5. Verify data sources in Grafana
# Go to Configuration > Data Sources
# Expected: Loki, Tempo, Mimir configured

# 6. Test log query
# In Grafana Explore, select Loki:
# Query: {namespace="kube-system"}
# Expected: Logs from kube-system namespace

# 7. Test metric query
# In Grafana Explore, select Mimir:
# Query: up
# Expected: Metric results

# 8. Check ServiceMonitors
kubectl get servicemonitor -A
# Expected: ServiceMonitors for various components
```

---

### Phase 14-15: Network Policies and Alerting

#### Test Phase 14 (Network Policies)

```bash
# 1. Verify default deny policies
kubectl get networkpolicy -A
# Expected: Policies in each namespace

# 2. Test that allowed traffic works
kubectl run test-pod --image=busybox --rm -it --restart=Never -- wget -qO- http://grafana.observability.svc:80
# Expected: Should work (internal observability access)

# 3. Test that blocked traffic is denied
# Create a pod in a restricted namespace and try to access services it shouldn't
```

#### Test Phase 15 (Alerting)

```bash
# 1. Check Alertmanager pods
kubectl get pods -n observability -l app.kubernetes.io/name=alertmanager
# Expected: 3 Alertmanager pods (HA)

# 2. Access Alertmanager UI
kubectl port-forward -n observability svc/alertmanager 9093:9093 &
# Open http://localhost:9093

# 3. Verify alert rules
kubectl get prometheusrules -n observability
# Expected: Multiple PrometheusRule resources

# 4. Check for active alerts
kubectl port-forward -n observability svc/alertmanager 9093:9093 &
curl http://localhost:9093/api/v2/alerts
# Expected: JSON array of alerts (may be empty if all healthy)

# 5. Verify Alertmanager config was loaded
kubectl logs -n observability -l app.kubernetes.io/name=alertmanager | grep "Loading configuration file"
# Expected: Configuration loaded successfully
```

---

## Quick Reference: All Manual Secrets

| Secret Name | Namespace | Purpose | When to Create |
|-------------|-----------|---------|----------------|
| `cloudflare-api-token` | cert-manager | DNS01 challenges | Before Phase 9 |
| `proxmox-csi-credentials` | csi-proxmox | CSI driver auth | Before Phase 7 |
| `grafana-admin` | observability | Grafana login | Before Phase 13 |
| `alertmanager-config` | observability | Alert routing | Before Phase 15 |
| `spicedb-config` | spicedb | API authentication | Before Phase 12 |

---

## Troubleshooting Quick Commands

```bash
# Flux issues
flux logs --level=error --all-namespaces
flux get all

# Pod issues
kubectl describe pod <pod-name> -n <namespace>
kubectl logs <pod-name> -n <namespace>

# Certificate issues
kubectl describe certificate <name> -n <namespace>
kubectl get challenges -A
kubectl describe challenge <name> -n <namespace>

# Storage issues
kubectl describe pvc <name> -n <namespace>
kubectl logs -n csi-proxmox -l app=csi-proxmox-controller

# Network policy issues
kubectl describe networkpolicy <name> -n <namespace>

# Talos issues
talosctl -n <node-ip> logs kubelet
talosctl -n <node-ip> dmesg
talosctl -n <node-ip> health
```

---

## Success Checklist

- [ ] Proxmox API tokens created
- [ ] Environment file configured
- [ ] DNS records created
- [ ] Port forwarding configured
- [ ] VMs provisioned and healthy
- [ ] Kubernetes cluster accessible
- [ ] Cilium CNI operational
- [ ] CSI driver provisioning volumes
- [ ] Flux reconciling successfully
- [ ] Certificates issuing correctly
- [ ] Gateway routing traffic
- [ ] PostgreSQL clusters healthy
- [ ] SpiceDB responding to API calls
- [ ] Grafana accessible with data
- [ ] Alertmanager configured
- [ ] Network policies enforced
