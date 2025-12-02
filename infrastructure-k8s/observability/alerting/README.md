# Alerting Configuration

Prometheus alerting rules and Alertmanager configuration for the waddle-infra cluster.

## Architecture

```
┌─────────────┐    ┌──────────────┐    ┌───────────────┐
│  Prometheus │───▶│ Alertmanager │───▶│ Notifications │
│   Rules     │    │   Routing    │    │ (Slack/Email) │
└─────────────┘    └──────────────┘    └───────────────┘
       │                  │
       │                  ▼
       │           ┌──────────────┐
       ▼           │   Silence/   │
  ┌─────────┐      │  Inhibition  │
  │ Grafana │      └──────────────┘
  │ Alerts  │
  └─────────┘
```

## Alert Categories

| Category | File | Description |
|----------|------|-------------|
| Infrastructure | `infrastructure-alerts.yaml` | Node, pod, storage alerts |
| Cilium | `cilium-alerts.yaml` | CNI and network policy alerts |
| cert-manager | `cert-manager-alerts.yaml` | Certificate lifecycle alerts |
| CloudNativePG | `cnpg-alerts.yaml` | PostgreSQL database alerts |
| Flux | `flux-alerts.yaml` | GitOps reconciliation alerts |

## Severity Levels

| Severity | Description | SLO | Response Time |
|----------|-------------|-----|---------------|
| **critical** | Immediate action required, service impacting | 99.9% | < 15 min |
| **warning** | Investigation needed, potential issue | 99.5% | < 1 hour |
| **info** | Informational, no immediate action | N/A | Next business day |

## Notification Channels

Configure in `alertmanager-config-secret.example.yaml`:

- **Slack**: Real-time alerts for critical/warning
- **Email**: Daily digest of all alerts
- **PagerDuty**: Critical alerts with on-call escalation

## Alert Routing

```yaml
route:
  group_by: ['alertname', 'namespace']
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h
  receiver: 'slack-default'
  routes:
    - match:
        severity: critical
      receiver: 'pagerduty-critical'
    - match:
        severity: warning
      receiver: 'slack-warnings'
```

## Silencing Alerts

### Via Alertmanager UI

```bash
kubectl port-forward -n observability svc/alertmanager 9093:9093
# Open http://localhost:9093/#/silences
```

### Via amtool CLI

```bash
# Silence specific alert for 2 hours
amtool silence add alertname=NodeMemoryPressure --duration=2h

# List active silences
amtool silence query

# Expire a silence
amtool silence expire <silence-id>
```

## Testing Alerts

### Verify PrometheusRules syntax

```bash
# Check rules are loaded
kubectl get prometheusrules -n observability

# Describe specific rule
kubectl describe prometheusrule infrastructure-alerts -n observability
```

### Trigger test alert

```bash
# Create a test alert (requires amtool or curl)
curl -X POST http://localhost:9093/api/v2/alerts \
  -H 'Content-Type: application/json' \
  -d '[{
    "labels": {"alertname": "TestAlert", "severity": "info"},
    "annotations": {"summary": "Test alert from CLI"}
  }]'
```

## Troubleshooting

### Alert not firing

1. Check PrometheusRule is loaded:
   ```bash
   kubectl get prometheusrules -n observability
   ```
2. Verify metric exists:
   ```bash
   kubectl port-forward -n observability svc/prometheus 9090:9090
   # Query metric in Prometheus UI
   ```
3. Check rule syntax in Prometheus:
   ```bash
   # Open http://localhost:9090/rules
   ```

### Notification not sent

1. Check Alertmanager logs:
   ```bash
   kubectl logs -n observability -l app.kubernetes.io/name=alertmanager
   ```
2. Verify alertmanager-config Secret exists:
   ```bash
   kubectl get secret alertmanager-config -n observability
   ```
3. Test notification channel manually (webhook URL, SMTP)

### Alert flapping

1. Increase `for` duration in PrometheusRule
2. Adjust `group_wait` and `group_interval` in Alertmanager config
3. Consider adding inhibition rules

## Runbook Links

Each alert includes a `runbook_url` annotation linking to:
- `docs/runbooks/node-down.md`
- `docs/runbooks/pod-crashlooping.md`
- `docs/runbooks/certificate-expiring.md`
- `docs/runbooks/postgresql-down.md`

## References

- [Prometheus Alerting Rules](https://prometheus.io/docs/prometheus/latest/configuration/alerting_rules/)
- [Alertmanager Configuration](https://prometheus.io/docs/alerting/latest/configuration/)
- [Alertmanager Routing Tree](https://prometheus.io/docs/alerting/latest/configuration/#route)
