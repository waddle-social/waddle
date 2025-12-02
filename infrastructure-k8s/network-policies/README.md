# Network Policies

Zero-trust networking implementation using Kubernetes NetworkPolicy resources enforced by Cilium CNI.

## Architecture

This directory implements a **default-deny** network security model where all traffic is blocked unless explicitly allowed. This follows the zero-trust principle of "never trust, always verify."

### Policy Hierarchy

1. **Default Deny** - Block all ingress and egress traffic
2. **DNS Allow** - Enable DNS resolution cluster-wide
3. **Component-Specific Allow** - Explicit rules per namespace/component

## Policies

| Policy | Namespace | Description |
|--------|-----------|-------------|
| `default-deny-all.yaml` | All (except kube-system, flux-system) | Block all traffic by default |
| `allow-dns.yaml` | All | Allow DNS queries to CoreDNS |
| `observability-ingress.yaml` | observability | Allow metric/log/trace ingestion |
| `spicedb-ingress.yaml` | spicedb | Allow API access and DB connections |
| `cnpg-ingress.yaml` | cnpg-system | Allow PostgreSQL and replication traffic |
| `cert-manager-egress.yaml` | cert-manager | Allow ACME and Cloudflare API access |
| `gateway-ingress.yaml` | gateway-ingress | Allow external traffic and backend routing |

## Namespace Isolation

```
┌─────────────────────────────────────────────────────────────────┐
│                        Kubernetes Cluster                        │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │ kube-system │    │ flux-system │    │ observability│         │
│  │ (exempt)    │    │ (exempt)    │    │              │         │
│  └─────────────┘    └─────────────┘    └──────┬──────┘         │
│                                                │                 │
│         DNS (53)                     metrics/logs/traces         │
│            ▲                                   │                 │
│            │                                   ▼                 │
│  ┌─────────┴───────────────────────────────────────────┐       │
│  │                  DEFAULT DENY                        │       │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐          │       │
│  │  │ spicedb  │  │ gateway  │  │ cert-mgr │          │       │
│  │  │          │  │ -ingress │  │          │          │       │
│  │  └──────────┘  └──────────┘  └──────────┘          │       │
│  └─────────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────────┘
```

## Verification

### Check deployed policies

```bash
# List all NetworkPolicies
kubectl get networkpolicies -A

# Describe specific policy
kubectl describe networkpolicy default-deny-all -n <namespace>
```

### Test connectivity with Cilium

```bash
# Run Cilium connectivity test
cilium connectivity test

# Check Cilium network policy status
cilium status --verbose
```

### Debug denied traffic

```bash
# Check Cilium policy verdicts
kubectl exec -n kube-system -it ds/cilium -- cilium monitor --type drop

# View policy enforcement
kubectl exec -n kube-system -it ds/cilium -- cilium policy get
```

## Troubleshooting

### Pods cannot resolve DNS

1. Verify allow-dns policy is applied:
   ```bash
   kubectl get networkpolicy allow-dns -n <namespace>
   ```
2. Check CoreDNS is running:
   ```bash
   kubectl get pods -n kube-system -l k8s-app=kube-dns
   ```

### Service cannot reach another service

1. Check if appropriate allow policy exists
2. Verify pod labels match policy selectors:
   ```bash
   kubectl get pods -n <namespace> --show-labels
   ```
3. Check Cilium policy enforcement:
   ```bash
   kubectl exec -n kube-system -it ds/cilium -- cilium endpoint list
   ```

### Metrics not being scraped

1. Verify observability-ingress policy allows scraping
2. Check ServiceMonitor selectors match target pods
3. Ensure metrics port is included in policy

## Adding New Policies

When adding a new application namespace:

1. **Apply default-deny** to the namespace (add namespace to policy)
2. **Create allow rules** for required ingress/egress:
   - DNS egress (usually covered by cluster-wide policy)
   - Application-specific ingress (API ports)
   - Database egress (if applicable)
   - Observability ingress (metrics scraping)
3. **Test connectivity** before deploying to production
4. **Document** the policy in this README

## References

- [Kubernetes Network Policies](https://kubernetes.io/docs/concepts/services-networking/network-policies/)
- [Cilium Network Policy](https://docs.cilium.io/en/stable/security/policy/)
- [Zero Trust Networking](https://www.cncf.io/blog/2021/03/25/zero-trust-architecture-for-kubernetes/)
