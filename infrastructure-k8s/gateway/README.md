# Gateway API Ingress

Gateway API configuration for HTTP/HTTPS ingress with TLS termination using Cilium GatewayClass and cert-manager for automated certificate management.

## Architecture

```
                                    ┌─────────────────────────────────────────────────────────┐
                                    │                   Kubernetes Cluster                     │
                                    │                                                          │
┌──────────┐   HTTPS    ┌──────────┴───────────┐         ┌──────────────────────────────┐    │
│          │  :443      │       Gateway         │         │      Application Namespace    │    │
│  Client  │───────────▶│   (gateway-ingress)   │────────▶│                               │    │
│          │            │                       │         │  ┌──────────┐  ┌──────────┐   │    │
└──────────┘            │  ┌─────────────────┐  │         │  │HTTPRoute │──│ Service  │   │    │
                        │  │ TLS Termination │  │         │  └──────────┘  └──────────┘   │    │
                        │  │ (gateway-tls)   │  │         │                     │         │    │
                        │  └─────────────────┘  │         │                     ▼         │    │
                        │                       │         │              ┌──────────┐     │    │
                        │  ┌─────────────────┐  │         │              │   Pods   │     │    │
                        │  │ Cilium Gateway  │  │         │              └──────────┘     │    │
                        │  │  Controller     │  │         │                               │    │
                        │  └─────────────────┘  │         └──────────────────────────────┘    │
                        └──────────────────────┘                                              │
                                    │                                                          │
                                    └─────────────────────────────────────────────────────────┘
```

## TLS Certificate Flow

```
┌─────────────┐    1. Create    ┌─────────────┐    2. ACME    ┌─────────────────┐
│   Gateway   │───Certificate──▶│ cert-manager│────Order─────▶│  Let's Encrypt  │
│  Resource   │                 │             │               │                 │
└─────────────┘                 └──────┬──────┘               └────────┬────────┘
                                       │                               │
                                       │ 3. Create TXT                 │ 4. Verify
                                       │    Record                     │    DNS
                                       ▼                               ▼
                                ┌─────────────┐               ┌─────────────────┐
                                │ Cloudflare  │◀──────────────│   DNS Lookup    │
                                │   DNS API   │               │                 │
                                └──────┬──────┘               └─────────────────┘
                                       │
                                       │ 5. Certificate
                                       │    Issued
                                       ▼
                                ┌─────────────┐
                                │   Secret    │
                                │ gateway-tls │
                                └─────────────┘
```

## Prerequisites

1. **Cilium CNI with Gateway API** (Phase 6)
   ```bash
   kubectl get gatewayclass cilium
   # Should show: cilium   io.cilium/gateway-controller   Accepted
   ```

2. **cert-manager with ClusterIssuers** (Phase 9)
   ```bash
   kubectl get clusterissuer
   # Should show: letsencrypt-staging and letsencrypt-production
   ```

3. **Cloudflare Domain**
   - Domain managed by Cloudflare
   - API token configured for cert-manager (Zone:DNS:Edit)

## Quick Start

### 1. Update Configuration

Edit `gateway.yaml` and `certificate.yaml` to replace `waddle.social` with your domain:

```bash
# Update domain in gateway.yaml
sed -i 's/waddle.social/waddle.social/g' gateway.yaml

# Update domain in certificate.yaml
sed -i 's/waddle.social/waddle.social/g' certificate.yaml
```

### 2. Deploy Gateway

```bash
# Create namespace and deploy resources
kubectl apply -f namespace.yaml
kubectl apply -f gateway.yaml
kubectl apply -f certificate.yaml
```

### 3. Get LoadBalancer IP

```bash
kubectl get gateway gateway -n gateway-ingress -o jsonpath='{.status.addresses[0].value}'
```

### 4. Configure DNS

In Cloudflare Dashboard:
1. Go to your domain's DNS settings
2. Create A record: `waddle.social` → `<LoadBalancer-IP>`
3. Create A record: `*.waddle.social` → `<LoadBalancer-IP>` (for wildcard)

### 5. Verify Certificate

```bash
# Check certificate status
kubectl get certificate -n gateway-ingress

# Wait for READY=True (may take 1-5 minutes)
kubectl describe certificate gateway-tls -n gateway-ingress
```

### 6. Test Access

```bash
# Test with curl (after DNS propagation)
curl -v https://waddle.social

# Or test with Host header before DNS
curl -k -H "Host: waddle.social" https://<LoadBalancer-IP>/
```

## File Descriptions

| File | Description |
|------|-------------|
| `namespace.yaml` | Gateway ingress namespace |
| `gateway.yaml` | Gateway resource with HTTP/HTTPS listeners |
| `certificate.yaml` | TLS certificate from Let's Encrypt |
| `kustomization.yaml` | Kustomize configuration for Flux |
| `verification/` | Test resources for Gateway verification |

## Verification

### Deploy Test Service

```bash
# Create test namespace and deploy echo service
kubectl apply -f verification/namespace.yaml
kubectl apply -f verification/echo-service.yaml
kubectl apply -f verification/test-httproute.yaml

# Update test-httproute.yaml with your domain first!
sed -i 's/waddle.social/waddle.social/g' verification/test-httproute.yaml
```

### Test Routing

```bash
# Get Gateway IP
export GATEWAY_IP=$(kubectl get gateway gateway -n gateway-ingress -o jsonpath='{.status.addresses[0].value}')

# Test HTTP (may redirect to HTTPS)
curl -H "Host: echo.waddle.social" http://$GATEWAY_IP/

# Test HTTPS
curl -k -H "Host: echo.waddle.social" https://$GATEWAY_IP/
# Expected: "Hello from Gateway API!"
```

### Test Staging Certificate First

```bash
# Always test with staging before production!
kubectl apply -f verification/test-certificate-staging.yaml

# Monitor certificate status
kubectl get certificate -n gateway-test -w

# Cleanup after testing
kubectl delete -f verification/test-certificate-staging.yaml
```

## HTTPRoute Patterns

### Path-Based Routing

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: api-route
  namespace: my-app
spec:
  parentRefs:
    - name: gateway
      namespace: gateway-ingress
  hostnames:
    - api.waddle.social
  rules:
    - matches:
        - path:
            type: PathPrefix
            value: /v1
      backendRefs:
        - name: api-v1
          port: 8080
    - matches:
        - path:
            type: PathPrefix
            value: /v2
      backendRefs:
        - name: api-v2
          port: 8080
```

### Host-Based Routing

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: multi-host
  namespace: my-app
spec:
  parentRefs:
    - name: gateway
      namespace: gateway-ingress
  hostnames:
    - app.waddle.social
    - www.waddle.social
  rules:
    - backendRefs:
        - name: web-app
          port: 8080
```

### HTTP to HTTPS Redirect

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: http-redirect
  namespace: gateway-ingress
spec:
  parentRefs:
    - name: gateway
      sectionName: http  # Attach to HTTP listener only
  hostnames:
    - "*.waddle.social"
  rules:
    - filters:
        - type: RequestRedirect
          requestRedirect:
            scheme: https
            statusCode: 301
```

## Troubleshooting

### Gateway Not Ready

```bash
# Check Gateway status
kubectl describe gateway gateway -n gateway-ingress

# Check Cilium Gateway controller
kubectl logs -n kube-system -l k8s-app=cilium -c cilium-agent | grep -i gateway

# Check GatewayClass
kubectl describe gatewayclass cilium
```

### Certificate Issues

```bash
# Check certificate status
kubectl describe certificate gateway-tls -n gateway-ingress

# Check challenges (if stuck)
kubectl get challenges -A
kubectl describe challenge <name> -n gateway-ingress

# Check cert-manager logs
kubectl logs -n cert-manager -l app=cert-manager

# Verify DNS TXT record (during challenge)
dig _acme-challenge.waddle.social TXT
```

### HTTPRoute Not Working

```bash
# Check HTTPRoute status
kubectl describe httproute <name> -n <namespace>

# Verify parentRef is accepted
# Status should show: "Accepted: True"

# Check if backend is reachable
kubectl port-forward svc/<service> -n <namespace> 8080:8080
curl localhost:8080
```

### LoadBalancer IP Not Assigned

```bash
# Check if MetalLB or Cilium IPAM is configured
kubectl get services -n gateway-ingress

# For Cilium, check IP pool
kubectl get ciliumbgpconfig -A
kubectl get ciliumloadbalancerippools -A
```

## Security Considerations

1. **Namespace Isolation**: Gateway resources are in dedicated `gateway-ingress` namespace
2. **RBAC**: Limit who can create/modify Gateway and HTTPRoute resources
3. **ReferenceGrant**: Use for explicit cross-namespace permissions in production
4. **TLS**: Always terminate TLS at Gateway, not passthrough
5. **Certificate Rotation**: cert-manager auto-renews 30 days before expiry

## Related Documentation

- [Gateway API Setup Guide](../../docs/gateway-api-setup.md)
- [cert-manager Configuration](../cert-manager/README.md)
- [Cilium Gateway API](../cilium/README.md)
- [Gateway API Specification](https://gateway-api.sigs.k8s.io/)
- [Cilium Gateway API Docs](https://docs.cilium.io/en/stable/network/servicemesh/gateway-api/gateway-api/)
