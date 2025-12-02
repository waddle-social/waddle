# Security Hardening Guide

Comprehensive security controls implemented and recommended for the waddle-infra Kubernetes cluster.

## Security Model

This infrastructure follows a **defense-in-depth** approach with multiple layers of security controls:

```
┌─────────────────────────────────────────────────────────────────┐
│                        External Access                          │
│                    (Teleport Zero-Trust)                        │
├─────────────────────────────────────────────────────────────────┤
│                      Network Layer                              │
│              (NetworkPolicies, Cilium CNI)                      │
├─────────────────────────────────────────────────────────────────┤
│                    Transport Layer                              │
│              (TLS via cert-manager)                             │
├─────────────────────────────────────────────────────────────────┤
│                   Application Layer                             │
│          (RBAC, Pod Security, Resource Limits)                  │
├─────────────────────────────────────────────────────────────────┤
│                      Data Layer                                 │
│        (Encryption at rest, Secrets Management)                 │
└─────────────────────────────────────────────────────────────────┘
```

## Implemented Security Controls

### 1. Network Segmentation

**Status**: ✅ Implemented (Phase 14)

NetworkPolicies enforce zero-trust networking:

- **Default Deny**: All traffic blocked unless explicitly allowed
- **Namespace Isolation**: Pods can only communicate with explicitly allowed namespaces
- **Component-Specific Rules**: Minimum necessary access per component

```bash
# Verify NetworkPolicies
kubectl get networkpolicies -A

# Check Cilium policy enforcement
kubectl exec -n kube-system -it ds/cilium -- cilium policy get
```

**Documentation**: `infrastructure-k8s/network-policies/README.md`

### 2. Access Control

**Status**: ✅ Implemented

**Teleport Zero-Trust Access:**
- MFA required for all access
- Session recording and audit logging
- Time-limited certificates (no long-lived credentials)
- RBAC roles: `kubernetes-admin`, `kubernetes-developer`

```bash
# Verify Teleport access
tsh status
kubectl auth can-i --list
```

**Kubernetes RBAC:**
- Least-privilege principle
- Role separation (admin vs developer)
- No cluster-admin for day-to-day operations

**Documentation**: `docs/teleport-setup.md`

### 3. Secrets Management

**Status**: ✅ Implemented

**Current Approach:**
- No secrets committed to Git (`.gitignore` configured)
- Kubernetes Secrets for runtime credentials
- Manual secret creation documented with `.example.yaml` files

```bash
# Verify no secrets in Git
git log --all --full-history -- "*.secret.yaml" "*.secrets.yaml"

# Check .gitignore
grep -E "secret|password|key|token" .gitignore
```

### 4. TLS Encryption

**Status**: ✅ Implemented (Phase 9)

**cert-manager with Let's Encrypt:**
- Automated certificate issuance and renewal
- DNS01 challenges via Cloudflare
- TLS for all external-facing services

```bash
# Check certificate status
kubectl get certificates -A

# Verify TLS on Gateway
kubectl get gateway -A -o jsonpath='{.items[*].spec.listeners[*].tls}'
```

**Documentation**: `docs/cert-manager-setup.md`

### 5. Observability and Alerting

**Status**: ✅ Implemented (Phase 13, 15)

**Security-Relevant Monitoring:**
- Cilium policy denial monitoring
- Failed authentication attempts (via Teleport logs)
- Unusual network traffic patterns
- Certificate expiration alerts

```bash
# Check security-related alerts
kubectl get prometheusrules -n observability | grep -E "cilium|cert"
```

## Recommended Enhancements

### 1. Pod Security Standards (PSS)

**Status**: ⏳ Recommended

Enforce security contexts at the namespace level:

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: production
  labels:
    pod-security.kubernetes.io/enforce: restricted
    pod-security.kubernetes.io/audit: restricted
    pod-security.kubernetes.io/warn: restricted
```

**Restricted PSS Requirements:**
- Non-root user
- Read-only root filesystem
- No privilege escalation
- Drop all capabilities (except NET_BIND_SERVICE if needed)

**Example Pod Security Context:**
```yaml
securityContext:
  runAsNonRoot: true
  runAsUser: 1000
  fsGroup: 1000
  seccompProfile:
    type: RuntimeDefault
containers:
  - name: app
    securityContext:
      allowPrivilegeEscalation: false
      readOnlyRootFilesystem: true
      capabilities:
        drop:
          - ALL
```

### 2. Resource Quotas and Limit Ranges

**Status**: ⏳ Recommended

Prevent resource exhaustion attacks:

```yaml
apiVersion: v1
kind: ResourceQuota
metadata:
  name: namespace-quota
  namespace: production
spec:
  hard:
    requests.cpu: "10"
    requests.memory: 20Gi
    limits.cpu: "20"
    limits.memory: 40Gi
    pods: "50"
---
apiVersion: v1
kind: LimitRange
metadata:
  name: default-limits
  namespace: production
spec:
  limits:
    - default:
        cpu: "500m"
        memory: "512Mi"
      defaultRequest:
        cpu: "100m"
        memory: "128Mi"
      type: Container
```

### 3. Image Security

**Status**: ⏳ Recommended

**Image Scanning:**
- Integrate Trivy or Clair for vulnerability scanning
- Scan images in CI/CD pipeline before deployment
- Block deployment of images with critical vulnerabilities

**Image Signing:**
- Use Sigstore/Cosign for image signing
- Enforce signature verification in admission controller

**Minimal Base Images:**
- Use distroless or Alpine-based images
- Avoid images with unnecessary packages

### 4. Secret Rotation

**Status**: ⏳ Recommended

**Automated Rotation Options:**
- **External Secrets Operator**: Sync secrets from Vault, AWS Secrets Manager
- **Sealed Secrets**: Git-safe encrypted secrets
- **Vault**: Full secrets management with rotation

**Certificate Rotation:**
- Teleport certificates: Auto-rotate via Teleport
- TLS certificates: Auto-rotate via cert-manager
- Database credentials: Manual rotation (document procedure)

### 5. Audit Logging

**Status**: ⏳ Recommended

**Kubernetes Audit Logging:**
```yaml
# audit-policy.yaml
apiVersion: audit.k8s.io/v1
kind: Policy
rules:
  - level: RequestResponse
    resources:
      - group: ""
        resources: ["secrets", "configmaps"]
  - level: Metadata
    resources:
      - group: ""
        resources: ["pods", "services"]
```

**Forward to Loki:**
- Configure audit log shipper
- Create Grafana dashboard for audit events
- Alert on suspicious patterns (excessive secret access, privilege escalation attempts)

## Compliance Mapping

### CIS Kubernetes Benchmark

| Control | Status | Notes |
|---------|--------|-------|
| 1.1 Control Plane Configuration | ✅ | Talos secure defaults |
| 1.2 API Server | ✅ | Talos managed |
| 4.1 Worker Node Configuration | ✅ | Talos secure defaults |
| 5.1 RBAC and Service Accounts | ✅ | Teleport RBAC |
| 5.2 Pod Security Standards | ⏳ | Recommended |
| 5.3 Network Policies | ✅ | Implemented |
| 5.4 Secrets Management | ✅ | Basic implementation |

### NIST Cybersecurity Framework

| Function | Controls | Status |
|----------|----------|--------|
| **Identify** | Asset inventory, risk assessment | ⏳ |
| **Protect** | Access control, data security, network segmentation | ✅ |
| **Detect** | Monitoring, anomaly detection | ✅ |
| **Respond** | Incident response, runbooks | ✅ |
| **Recover** | DR procedures, backup/restore | ✅ |

## Security Checklist

Use this checklist for security reviews:

### Access Control
- [ ] Teleport MFA enabled for all users
- [ ] RBAC roles follow least-privilege
- [ ] No long-lived credentials in use
- [ ] Service accounts have minimal permissions

### Network Security
- [ ] NetworkPolicies applied to all namespaces
- [ ] Default-deny policy in place
- [ ] External traffic only through Gateway
- [ ] No unnecessary ports exposed

### Application Security
- [ ] All containers run as non-root
- [ ] Read-only root filesystems where possible
- [ ] Resource limits set on all containers
- [ ] No privileged containers

### Data Security
- [ ] No secrets in Git repository
- [ ] TLS enabled for all external traffic
- [ ] Database connections encrypted
- [ ] Backups encrypted (if applicable)

### Monitoring
- [ ] Security alerts configured
- [ ] Audit logging enabled
- [ ] Anomaly detection rules in place
- [ ] Incident response runbooks available

## References

- [Teleport Setup](teleport-setup.md) - Security Best Practices section
- [Network Policies](../infrastructure-k8s/network-policies/README.md)
- [Kubernetes Security Best Practices](https://kubernetes.io/docs/concepts/security/overview/)
- [CIS Kubernetes Benchmark](https://www.cisecurity.org/benchmark/kubernetes)
- [NIST Cybersecurity Framework](https://www.nist.gov/cyberframework)
