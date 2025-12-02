# Certificate Expiring Runbook

## Alert

**Alert**: CertificateExpiringSoon7Days, CertificateExpiringSoon3Days, CertificateNotReady, CertificateRenewalFailed
**Severity**: Warning (7 days), Critical (3 days or renewal failed)
**Impact**: TLS connections will fail after certificate expiration, service outage

## Overview

A TLS certificate managed by cert-manager is approaching expiration or has failed to renew. If not addressed, HTTPS services will become unavailable.

## Diagnosis

### 1. Check Certificate Status

```bash
# List all certificates with expiration
kubectl get certificates -A

# Detailed certificate status
kubectl describe certificate <cert-name> -n <namespace>

# Check certificate readiness
kubectl get certificate <cert-name> -n <namespace> -o jsonpath='{.status.conditions}' | jq
```

### 2. Check Certificate Request

```bash
# List certificate requests
kubectl get certificaterequests -A

# Check for failed requests
kubectl get certificaterequests -A | grep -v True

# Describe failed request
kubectl describe certificaterequest <request-name> -n <namespace>
```

### 3. Check ACME Challenges

```bash
# List active challenges
kubectl get challenges -A

# Check challenge status
kubectl describe challenge <challenge-name> -n <namespace>

# Check challenge type and state
kubectl get challenges -A -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}: {.status.state}{"\n"}{end}'
```

### 4. Check cert-manager Logs

```bash
# cert-manager controller logs
kubectl logs -n cert-manager -l app=cert-manager -f

# cert-manager webhook logs
kubectl logs -n cert-manager -l app=webhook -f

# Filter for specific certificate
kubectl logs -n cert-manager -l app=cert-manager | grep <cert-name>
```

### 5. Check ClusterIssuer

```bash
# List ClusterIssuers
kubectl get clusterissuers

# Check ClusterIssuer status
kubectl describe clusterissuer letsencrypt-production

# Verify Cloudflare API token (check if secret exists)
kubectl get secret cloudflare-api-token -n cert-manager
```

## Common Causes

1. **ACME Rate Limiting**: Too many certificate requests to Let's Encrypt
2. **DNS Challenge Failure**: Cloudflare API issues, DNS propagation delay
3. **ClusterIssuer Misconfiguration**: Invalid API token, wrong email
4. **Network Issues**: Cannot reach Let's Encrypt ACME servers
5. **cert-manager Issues**: Controller crash, webhook unavailable

## Remediation

### Scenario 1: ACME Rate Limiting

Let's Encrypt has strict rate limits. If rate limited:

1. **Check rate limit status**: https://letsencrypt.org/docs/rate-limits/
2. **Wait for rate limit reset** (typically 1 week for cert limit)
3. **Use staging issuer for testing**:
   ```bash
   kubectl edit certificate <cert-name> -n <namespace>
   # Change issuerRef to letsencrypt-staging
   ```

### Scenario 2: DNS Challenge Failure

```bash
# Check DNS propagation
dig TXT _acme-challenge.<domain>

# Verify Cloudflare API token
kubectl get secret cloudflare-api-token -n cert-manager -o jsonpath='{.data.api-token}' | base64 -d

# Test Cloudflare API manually
curl -X GET "https://api.cloudflare.com/client/v4/user/tokens/verify" \
  -H "Authorization: Bearer <api-token>" \
  -H "Content-Type: application/json"
```

**Fix Cloudflare token if invalid:**
```bash
kubectl create secret generic cloudflare-api-token \
  --from-literal=api-token=<new-token> \
  -n cert-manager \
  --dry-run=client -o yaml | kubectl apply -f -
```

### Scenario 3: ClusterIssuer Misconfiguration

```bash
# Check ClusterIssuer status
kubectl describe clusterissuer letsencrypt-production

# Verify email is correct
kubectl get clusterissuer letsencrypt-production -o jsonpath='{.spec.acme.email}'

# Update if needed
kubectl edit clusterissuer letsencrypt-production
```

### Scenario 4: Network Issues

```bash
# Test connectivity to Let's Encrypt
kubectl run -it --rm test-net --image=curlimages/curl -- \
  curl -v https://acme-v02.api.letsencrypt.org/directory

# Check network policies
kubectl get networkpolicies -n cert-manager
```

### Scenario 5: cert-manager Issues

```bash
# Restart cert-manager
kubectl rollout restart deployment cert-manager -n cert-manager
kubectl rollout restart deployment cert-manager-webhook -n cert-manager

# Check for OOM or resource issues
kubectl describe pod -n cert-manager -l app=cert-manager

# Verify CRDs are installed
kubectl get crd | grep cert-manager
```

### Manual Certificate Renewal

If automatic renewal fails, manually trigger renewal:

```bash
# Delete the Certificate resource (will be recreated)
kubectl delete certificate <cert-name> -n <namespace>

# Or delete the secret to force re-issuance
kubectl delete secret <cert-secret-name> -n <namespace>

# Force certificate recreation
kubectl annotate certificate <cert-name> -n <namespace> \
  cert-manager.io/issuer-name-
kubectl annotate certificate <cert-name> -n <namespace> \
  cert-manager.io/issuer-name=letsencrypt-production
```

### Emergency: Manual Certificate Creation

If cert-manager is completely broken and certificate expires:

```bash
# Generate temporary self-signed certificate
openssl req -x509 -newkey rsa:4096 -keyout tls.key -out tls.crt -days 7 -nodes \
  -subj "/CN=<domain>"

# Create Kubernetes secret
kubectl create secret tls <cert-secret-name> \
  --cert=tls.crt \
  --key=tls.key \
  -n <namespace>
```

**Note**: Self-signed certs will show browser warnings. This is a temporary measure only.

## Verification

After remediation:

```bash
# Verify certificate is ready
kubectl get certificate <cert-name> -n <namespace>

# Check expiration date
kubectl get secret <cert-secret-name> -n <namespace> -o jsonpath='{.data.tls\.crt}' | \
  base64 -d | openssl x509 -noout -enddate

# Test HTTPS endpoint
curl -v https://<domain>

# Check certificate details
echo | openssl s_client -servername <domain> -connect <domain>:443 2>/dev/null | \
  openssl x509 -noout -dates
```

## Escalation

- **7 days to expiry**: Investigate and resolve during business hours
- **3 days to expiry**: Priority fix, consider manual renewal
- **24 hours to expiry**: Emergency, create temporary certificate if needed
- **Expired**: Immediate action, manual certificate or service downtime

## Related Documentation

- [cert-manager Setup](../cert-manager-setup.md)
- [Cloudflare DNS Setup](https://developers.cloudflare.com/api/tokens/)
- [Let's Encrypt Rate Limits](https://letsencrypt.org/docs/rate-limits/)
- [cert-manager Troubleshooting](https://cert-manager.io/docs/troubleshooting/)
