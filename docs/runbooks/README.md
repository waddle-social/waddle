# Operational Runbooks

Step-by-step procedures for responding to alerts and common operational scenarios.

## Purpose

Runbooks provide documented procedures for:
- **Incident Response**: Quick diagnosis and remediation of alerts
- **Troubleshooting**: Systematic approaches to common issues
- **Knowledge Sharing**: Consistent procedures across team members
- **On-call Support**: Reference material for on-call engineers

## Runbook Structure

Each runbook follows a standard format:

```markdown
## Alert Name

**Alert**: Alert name from PrometheusRule
**Severity**: Critical | Warning | Info
**Impact**: Business/technical impact of the issue

### Diagnosis
1. Step-by-step investigation commands
2. What to look for in logs/metrics
3. Common root causes

### Remediation
1. Immediate actions to restore service
2. Temporary workarounds
3. Permanent fixes

### Escalation
When and how to escalate if remediation fails
```

## Available Runbooks

### Infrastructure

| Runbook | Alert | Severity | Description |
|---------|-------|----------|-------------|
| [node-down.md](node-down.md) | NodeDown, NodeNotReady | Critical | Node unreachable or not ready |
| [pod-crashlooping.md](pod-crashlooping.md) | PodCrashLooping, PodOOMKilled | Warning | Pod restart loops |

### Certificates

| Runbook | Alert | Severity | Description |
|---------|-------|----------|-------------|
| [certificate-expiring.md](certificate-expiring.md) | CertificateExpiringSoon | Warning/Critical | TLS certificate expiration |

### Databases

| Runbook | Alert | Severity | Description |
|---------|-------|----------|-------------|
| [postgresql-down.md](postgresql-down.md) | PostgreSQLDown | Critical | PostgreSQL cluster failure |

## On-Call Procedures

### Severity Response Times

| Severity | Response Time | Action |
|----------|---------------|--------|
| Critical | < 15 minutes | Immediate investigation and remediation |
| Warning | < 1 hour | Investigation during business hours |
| Info | Next business day | Review and address as time permits |

### Escalation Path

1. **L1 - On-Call Engineer**: First responder, follows runbook
2. **L2 - Platform Team**: Escalation for complex issues
3. **L3 - Infrastructure Lead**: Final escalation for critical incidents

### Communication

- **Slack**: `#alerts-critical` for real-time updates
- **Incident Channel**: Create `#incident-YYYYMMDD-brief` for major incidents
- **Status Page**: Update for customer-facing impact

## Contributing New Runbooks

1. Create new runbook file in this directory
2. Follow the standard format above
3. Link to relevant documentation and dashboards
4. Add entry to the "Available Runbooks" table
5. Update PrometheusRule `runbook_url` annotation

## Testing Runbooks

Periodically test runbooks to ensure:
- Commands are accurate and work as expected
- Procedures are complete and clear
- Contact information is current
- Linked resources are accessible

## References

- [Alert Configuration](../../infrastructure-k8s/observability/alerting/README.md)
- [Disaster Recovery Testing](../disaster-recovery-testing.md)
- [Security Hardening](../security-hardening.md)
