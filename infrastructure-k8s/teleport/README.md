# Teleport Kubernetes Integration

This directory contains Kubernetes manifests for deploying the Teleport Kube Agent, which connects the Kubernetes cluster to the Teleport Auth/Proxy services for secure access.

## Architecture

**Teleport Components:**
- **Auth + Proxy Services**: Running on dedicated VM (provisioned in Phase 5a)
- **Kube Agent**: Deployed in this Kubernetes cluster (Phase 6+)

**Connection Flow:**
```
User → Teleport Proxy (VM) → Kube Agent (K8s) → Kubernetes API
```

The Kube Agent establishes a reverse tunnel to the Proxy, eliminating the need to expose the Kubernetes API publicly.

## Prerequisites

1. **Teleport VM deployed** (Phase 5a complete)
2. **Cilium CNI installed** (Phase 6 complete)
3. **Teleport Auth Service accessible** from Kubernetes cluster
4. **Join token generated** on Teleport Auth Service

## Deployment (Phase 6+)

### 1. Generate Join Token

SSH to the Teleport VM and create a join token for the Kube Agent:

```bash
ssh admin@{teleport-ip}
sudo tctl tokens add --type=kube --ttl=1h
```

Copy the generated token.

### 2. Configure Authentication

Choose one of the following authentication methods:

**Option A: Direct token in values file (simpler, less secure)**

Edit `helm-values.yaml` and set `authToken` directly:
```yaml
authToken: "<your-join-token-from-step-1>"
```

**Option B: Kubernetes Secret (recommended for production)**

Create a secret with the join token:
```bash
kubectl create namespace teleport
kubectl create secret generic teleport-kube-agent-join-token \
  --from-literal=auth-token=<your-join-token-from-step-1> \
  -n teleport
```

Then use `joinTokenSecret` in `helm-values.yaml` (this is the default configuration):
```yaml
joinTokenSecret:
  name: teleport-kube-agent-join-token
```

### 3. Deploy via Helm

Edit `helm-values.yaml` to set your Teleport domain and authentication method, then deploy:

```bash
helm repo add teleport https://charts.releases.teleport.dev
helm repo update
helm install teleport-kube-agent teleport/teleport-kube-agent \
  --namespace teleport \
  --values helm-values.yaml
```

### 4. Verify Deployment

```bash
kubectl get pods -n teleport
kubectl logs -n teleport -l app=teleport-kube-agent
```

### 5. Configure Kubernetes Access in Teleport

On the Teleport VM, create a role for Kubernetes access:

```bash
sudo tctl create -f /etc/teleport/roles/kubernetes-admin.yaml
```

See `roles/` directory for example role definitions.

## Access Kubernetes via Teleport

Once configured, users access Kubernetes through Teleport:

```bash
# Login to Teleport
tsh login --proxy={teleport-domain}:443 --user={username}

# List available Kubernetes clusters
tsh kube ls

# Login to Kubernetes cluster
tsh kube login {cluster-name}

# Use kubectl normally
kubectl get nodes
kubectl get pods -A
```

## RBAC Configuration

Teleport roles map to Kubernetes RBAC:

- **Teleport Role**: Defines which Kubernetes clusters and namespaces a user can access
- **Kubernetes Groups**: Teleport injects groups into kubectl requests
- **Kubernetes RoleBindings**: Grant permissions to groups

Example flow:
1. User has Teleport role `kubernetes-developer`
2. Role specifies `kubernetes_groups: ["developers"]`
3. Kubernetes has `RoleBinding` granting `developers` group edit access to `dev` namespace
4. User can edit resources in `dev` namespace via Teleport

See `roles/` directory for example configurations.

## Security Features

- **Per-Session MFA**: Require MFA for each kubectl session
- **Session Recording**: Record all kubectl commands and API requests
- **Audit Logging**: Comprehensive logs of all Kubernetes access
- **Just-in-Time Access**: Temporary elevated permissions via Access Requests
- **Moderated Sessions**: Require approval for production cluster access

## Troubleshooting

**Agent not connecting:**
- Verify Teleport Proxy is reachable from Kubernetes cluster
- Check join token is valid (not expired)
- Review agent logs: `kubectl logs -n teleport -l app=teleport-kube-agent`

**kubectl access denied:**
- Verify Teleport role includes correct `kubernetes_groups`
- Check Kubernetes RoleBindings for those groups
- Review Teleport audit logs: `sudo journalctl -u teleport -f`

**Certificate errors:**
- Ensure Teleport domain has valid TLS certificate
- Verify CA certificate is trusted

## Files in This Directory

- `README.md`: This file
- `helm-values.yaml`: Helm chart values for Kube Agent deployment
- `roles/`: Example Teleport roles for Kubernetes access

**Note:** Additional resources (`kustomization.yaml`, `rbac/`) may be added in future phases for declarative deployment and Kubernetes RBAC integration.

## References

- [Teleport Kubernetes Access](https://goteleport.com/docs/kubernetes-access/)
- [Teleport Kube Agent Helm Chart](https://goteleport.com/docs/kubernetes-access/helm/)
- [Kubernetes RBAC](https://kubernetes.io/docs/reference/access-authn-authz/rbac/)
