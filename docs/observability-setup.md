# Observability Stack Setup Guide

This guide covers deploying and configuring the LGTM observability stack (Loki, Grafana, Tempo, Mimir) with OpenTelemetry Collector for unified telemetry collection.

## Table of Contents

- [Introduction](#introduction)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Component Overview](#component-overview)
- [Manual Installation](#manual-installation)
- [Flux Deployment](#flux-deployment)
- [Grafana Access](#grafana-access)
- [Data Source Configuration](#data-source-configuration)
- [Dashboard Usage](#dashboard-usage)
- [Querying Data](#querying-data)
- [ServiceMonitor Configuration](#servicemonitor-configuration)
- [Log Aggregation](#log-aggregation)
- [Trace Instrumentation](#trace-instrumentation)
- [Alerting](#alerting)
- [Storage Management](#storage-management)
- [Performance Tuning](#performance-tuning)
- [Troubleshooting](#troubleshooting)
- [Security](#security)
- [Backup and Recovery](#backup-and-recovery)
- [Verification](#verification)
- [References](#references)

## Introduction

The observability stack provides comprehensive monitoring capabilities:

- **Logs**: Centralized log aggregation with full-text search and filtering
- **Metrics**: Time-series metrics storage with PromQL query support
- **Traces**: Distributed tracing for request flow visualization

All telemetry is collected by OpenTelemetry Collector and forwarded to specialized backends (Loki, Mimir, Tempo), with Grafana providing unified visualization.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        Applications / Infrastructure                         │
│  (Cilium, cert-manager, CloudNativePG, SpiceDB, custom applications)        │
└───────────────────────────────┬─────────────────────────────────────────────┘
                                │
            ┌───────────────────┼───────────────────┐
            │ logs              │ metrics           │ traces
            │ (stdout/stderr)   │ (Prometheus)      │ (OTLP)
            ▼                   ▼                   ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                       OpenTelemetry Collector (DaemonSet)                    │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐              │
│  │ filelog receiver│  │prometheus recv  │  │  otlp receiver  │              │
│  │ (/var/log/pods) │  │(ServiceMonitors)│  │  (gRPC/HTTP)    │              │
│  └────────┬────────┘  └────────┬────────┘  └────────┬────────┘              │
│           │                    │                    │                        │
│  ┌────────┴────────────────────┴────────────────────┴────────┐              │
│  │  Processors: batch, memory_limiter, k8sattributes, resource│              │
│  └────────┬────────────────────┬────────────────────┬────────┘              │
│           ▼                    ▼                    ▼                        │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐              │
│  │  loki exporter  │  │prometheusrw exp │  │  otlp exporter  │              │
│  └────────┬────────┘  └────────┬────────┘  └────────┬────────┘              │
└───────────┼────────────────────┼────────────────────┼────────────────────────┘
            │                    │                    │
            ▼                    ▼                    ▼
     ┌───────────┐        ┌───────────┐        ┌───────────┐
     │   Loki    │        │   Mimir   │        │   Tempo   │
     │  (Logs)   │        │ (Metrics) │        │ (Traces)  │
     │  50Gi PVC │        │ 100Gi PVC │        │  30Gi PVC │
     └─────┬─────┘        └─────┬─────┘        └─────┬─────┘
           │                    │                    │
           └────────────────────┼────────────────────┘
                                │
                                ▼
                         ┌───────────┐
                         │  Grafana  │
                         │ (Dashbd.) │
                         │  10Gi PVC │
                         └─────┬─────┘
                               │
                               ▼
                        User / Operator
```

### Data Flow

1. **Log Collection**: OTel Collector's filelog receiver reads container logs from `/var/log/pods`, enriches with Kubernetes metadata, and pushes to Loki
2. **Metric Scraping**: OTel Collector scrapes Prometheus endpoints discovered via ServiceMonitors and forwards to Mimir
3. **Trace Collection**: Applications send traces via OTLP (gRPC/HTTP) to OTel Collector, which forwards to Tempo
4. **Visualization**: Grafana queries all three backends for unified dashboards and exploration

## Prerequisites

Before deploying the observability stack:

1. **Phase 6 Complete**: Cilium CNI operational
2. **Phase 7 Complete**: Proxmox CSI driver installed (provides PVC storage)
3. **Phase 8 Complete**: Flux GitOps configured
4. **kubectl** configured with cluster access
5. **helm** v3.x installed (for manual installation only)

## Component Overview

| Component | Purpose | Chart | Storage |
|-----------|---------|-------|---------|
| **Prometheus Operator** | CRD management (ServiceMonitor/PodMonitor) | kube-prometheus-stack | None |
| **Loki** | Log aggregation | grafana/loki | 50Gi |
| **Tempo** | Distributed tracing | grafana/tempo | 30Gi |
| **Mimir** | Metrics storage (Prometheus-compatible) | grafana/mimir-distributed | 100Gi |
| **OTel Collector** | Unified telemetry collection | opentelemetry-collector | None |
| **Grafana** | Visualization and dashboards | grafana/grafana | 10Gi |

## Manual Installation

For manual installation without Flux:

### 1. Create Namespace and Secret

```bash
# Create namespace
kubectl create namespace observability

# Create Grafana admin password
kubectl create secret generic grafana-admin \
  --from-literal=admin-user=admin \
  --from-literal=admin-password=$(openssl rand -base64 32) \
  -n observability
```

### 2. Add Helm Repositories

```bash
helm repo add prometheus-community https://prometheus-community.github.io/helm-charts
helm repo add grafana https://grafana.github.io/helm-charts
helm repo add open-telemetry https://open-telemetry.github.io/opentelemetry-helm-charts
helm repo update
```

### 3. Deploy Components

```bash
# Deploy in order (respecting dependencies)

# 1. Prometheus Operator (CRDs)
helm install prometheus-operator prometheus-community/kube-prometheus-stack \
  -n observability \
  -f infrastructure-k8s/observability/prometheus-operator/helm-values.yaml \
  --wait

# 2. Storage backends (can deploy in parallel)
helm install loki grafana/loki \
  -n observability \
  -f infrastructure-k8s/observability/loki/helm-values.yaml \
  --wait

helm install tempo grafana/tempo \
  -n observability \
  -f infrastructure-k8s/observability/tempo/helm-values.yaml \
  --wait

helm install mimir grafana/mimir-distributed \
  -n observability \
  -f infrastructure-k8s/observability/mimir/helm-values.yaml \
  --wait

# 3. OpenTelemetry Collector (after backends)
helm install otel-collector open-telemetry/opentelemetry-collector \
  -n observability \
  -f infrastructure-k8s/observability/opentelemetry/helm-values.yaml \
  --wait

# 4. Grafana (after backends)
helm install grafana grafana/grafana \
  -n observability \
  -f infrastructure-k8s/observability/grafana/helm-values.yaml \
  --wait
```

### 4. Deploy ServiceMonitors and Dashboards

```bash
# ServiceMonitors
kubectl apply -k infrastructure-k8s/observability/servicemonitors/

# Dashboards
kubectl apply -k infrastructure-k8s/observability/dashboards/
```

## Flux Deployment

With Flux GitOps, the observability stack deploys automatically:

### Check Deployment Status

```bash
# HelmReleases status
flux get helmreleases -n observability

# Kustomizations status
flux get kustomizations | grep observability

# All pods running
kubectl get pods -n observability

# PVCs provisioned
kubectl get pvc -n observability
```

### Expected Output

```
NAME                  READY   STATUS
prometheus-operator   True    Release reconciliation succeeded
loki                  True    Release reconciliation succeeded
tempo                 True    Release reconciliation succeeded
mimir                 True    Release reconciliation succeeded
otel-collector        True    Release reconciliation succeeded
grafana               True    Release reconciliation succeeded
```

## Grafana Access

### Port-Forward (Development)

```bash
kubectl port-forward -n observability svc/grafana 3000:80
```

Open http://localhost:3000

### Get Admin Password

```bash
kubectl get secret grafana-admin -n observability \
  -o jsonpath='{.data.admin-password}' | base64 -d && echo
```

### Gateway API (Production)

Create an HTTPRoute for external access:

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: grafana
  namespace: observability
spec:
  parentRefs:
    - name: main-gateway
      namespace: gateway
  hostnames:
    - grafana.waddle.social
  rules:
    - matches:
        - path:
            type: PathPrefix
            value: /
      backendRefs:
        - name: grafana
          port: 80
```

## Data Source Configuration

Data sources are pre-configured in `grafana/helm-values.yaml`:

| Data Source | Type | URL |
|-------------|------|-----|
| Loki | loki | http://loki.observability.svc.cluster.local:3100 |
| Tempo | tempo | http://tempo.observability.svc.cluster.local:3100 |
| Mimir | prometheus | http://mimir-nginx.observability.svc.cluster.local:80/prometheus |

Verify in Grafana: **Configuration → Data Sources**

## Dashboard Usage

Pre-installed dashboards (via ConfigMaps):

| Dashboard | Description |
|-----------|-------------|
| Kubernetes Cluster Overview | Cluster resource usage, node health, pod counts |
| Cilium Network Observability | Network flows, drops, policy verdicts |
| Talos Node Metrics | Node CPU, memory, disk, network |
| CloudNativePG PostgreSQL | Database size, connections, replication lag |

Access: **Dashboards → Browse** or search by name

## Querying Data

### Logs (Loki - LogQL)

Navigate to **Explore → Loki**

```logql
# All logs from namespace
{namespace="observability"}

# Error logs
{} |= "error" or |= "ERROR"

# Logs from specific pod
{namespace="kube-system", pod=~"cilium.*"}

# Rate of logs
rate({namespace="observability"}[5m])

# JSON parsing
{namespace="default"} | json | level="error"
```

### Metrics (Mimir - PromQL)

Navigate to **Explore → Mimir**

```promql
# Cluster CPU usage
100 - (avg(rate(node_cpu_seconds_total{mode="idle"}[5m])) * 100)

# Memory usage by pod
sum(container_memory_working_set_bytes{namespace="observability"}) by (pod)

# Cilium drops
rate(cilium_drop_count_total[5m])

# Certificate expiry (days)
(certmanager_certificate_expiration_timestamp_seconds - time()) / 86400

# PostgreSQL connections
cnpg_pg_stat_activity_count
```

### Traces (Tempo - TraceQL)

Navigate to **Explore → Tempo**

```traceql
# All traces
{}

# Error traces
{ status = error }

# Slow traces
{ duration > 1s }

# By service
{ resource.service.name = "my-service" }
```

## ServiceMonitor Configuration

### Existing ServiceMonitors

| Resource | Target | Metrics Endpoint |
|----------|--------|------------------|
| cilium | Cilium Agent | :9962/metrics |
| cert-manager | cert-manager Controller | :9402/metrics |
| cloudnative-pg | CNPG Operator | /metrics |

### Adding New ServiceMonitors

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: my-service
  namespace: my-namespace
  labels:
    app.kubernetes.io/part-of: observability
spec:
  selector:
    matchLabels:
      app: my-service
  endpoints:
    - port: metrics
      interval: 30s
      path: /metrics
```

## Log Aggregation

OpenTelemetry Collector runs as a DaemonSet and collects logs from:

- Container stdout/stderr via `/var/log/pods`
- Kubernetes metadata enrichment (pod, namespace, labels)

### Log Format Support

- JSON logs: Automatically parsed
- Plain text: Stored as-is
- Multi-line: Handled by container runtime

### Log Filtering

Configure in OTel Collector `helm-values.yaml`:

```yaml
config:
  receivers:
    filelog:
      exclude:
        - /var/log/pods/*/otel-collector/*.log  # Exclude self
        - /var/log/pods/kube-system/kube-proxy*/*.log  # Example exclusion
```

## Trace Instrumentation

### Application Requirements

Applications must:
1. Include OpenTelemetry SDK
2. Configure OTLP exporter
3. Send traces to OTel Collector endpoint

### Collector Endpoints

| Protocol | Endpoint |
|----------|----------|
| OTLP gRPC | otel-collector-opentelemetry-collector.observability.svc.cluster.local:4317 |
| OTLP HTTP | otel-collector-opentelemetry-collector.observability.svc.cluster.local:4318 |

### Example Configuration (Python)

```python
from opentelemetry import trace
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor

provider = TracerProvider()
processor = BatchSpanProcessor(OTLPSpanExporter(
    endpoint="otel-collector-opentelemetry-collector.observability.svc.cluster.local:4317",
    insecure=True
))
provider.add_span_processor(processor)
trace.set_tracer_provider(provider)
```

## Alerting

Alerting is configured via Alertmanager and PrometheusRules (Phase 15).

### Prerequisites

- Prometheus Operator deployed (Phase 13)
- `alertmanager-config` Secret with notification credentials

### Creating Alertmanager Configuration

1. **Copy the example configuration:**
   ```bash
   cp infrastructure-k8s/observability/alerting/alertmanager-config-secret.example.yaml \
      alertmanager-config.yaml
   ```

2. **Edit with your notification credentials:**
   ```yaml
   global:
     slack_api_url: 'https://hooks.slack.com/services/YOUR/WEBHOOK/URL'
     smtp_smarthost: 'smtp.gmail.com:587'
     smtp_from: 'alerts@waddle.social'
   
   route:
     receiver: 'slack-default'
     group_by: ['alertname', 'namespace']
     routes:
       - match:
           severity: critical
         receiver: 'pagerduty-critical'
   
   receivers:
     - name: 'slack-default'
       slack_configs:
         - channel: '#alerts'
           send_resolved: true
     - name: 'pagerduty-critical'
       pagerduty_configs:
         - service_key: '<your-pagerduty-key>'
   ```

3. **Create the Secret:**
   ```bash
   kubectl create secret generic alertmanager-config \
     --from-file=alertmanager.yaml=alertmanager-config.yaml \
     -n observability
   ```

### Deploying PrometheusRules

PrometheusRules are deployed automatically via Flux:

```bash
# Check deployed rules
kubectl get prometheusrules -n observability

# View rule details
kubectl describe prometheusrule infrastructure-alerts -n observability
```

### Alert Categories

| Category | File | Key Alerts |
|----------|------|------------|
| Infrastructure | `infrastructure-alerts.yaml` | NodeDown, PodCrashLooping, PVCNearlyFull |
| Cilium | `cilium-alerts.yaml` | CiliumAgentDown, CiliumHighPolicyDenialRate |
| cert-manager | `cert-manager-alerts.yaml` | CertificateExpiringSoon, CertificateRenewalFailed |
| CloudNativePG | `cnpg-alerts.yaml` | PostgreSQLDown, PostgreSQLReplicationLag, PostgreSQLBackupFailed |
| Flux | `flux-alerts.yaml` | FluxReconciliationFailure, FluxSourceNotReady |

### Accessing Alertmanager

```bash
# Port-forward to Alertmanager
kubectl port-forward -n observability svc/alertmanager 9093:9093

# Open http://localhost:9093
```

### Testing Alerts

1. **Verify Alertmanager is running:**
   ```bash
   kubectl get pods -n observability -l app.kubernetes.io/name=alertmanager
   ```

2. **Send test alert:**
   ```bash
   curl -X POST http://localhost:9093/api/v2/alerts \
     -H 'Content-Type: application/json' \
     -d '[{
       "labels": {"alertname": "TestAlert", "severity": "info"},
       "annotations": {"summary": "Test alert"}
     }]'
   ```

3. **Verify notification received** in your Slack channel or email.

### Silencing Alerts

**Via Alertmanager UI:**
1. Open http://localhost:9093/#/silences
2. Click "New Silence"
3. Add matchers (e.g., `alertname=NodeMemoryPressure`)
4. Set duration and comment

**Via amtool CLI:**
```bash
# Silence specific alert
amtool silence add alertname=NodeMemoryPressure --duration=2h --comment="Investigating"

# List silences
amtool silence query

# Expire silence
amtool silence expire <silence-id>
```

### Runbooks

Each alert includes a `runbook_url` annotation linking to operational procedures:

- `docs/runbooks/node-down.md` - Node failures
- `docs/runbooks/pod-crashlooping.md` - Pod restart issues
- `docs/runbooks/certificate-expiring.md` - Certificate lifecycle
- `docs/runbooks/postgresql-down.md` - Database issues

See `docs/runbooks/README.md` for complete list.

### Troubleshooting Alerts

**Alert not firing:**
1. Check PrometheusRule syntax:
   ```bash
   kubectl describe prometheusrule <name> -n observability
   ```
2. Verify metric exists in Mimir:
   ```promql
   # Query in Grafana Explore
   up{job="cilium"}
   ```

**Notification not sent:**
1. Check Alertmanager logs:
   ```bash
   kubectl logs -n observability -l app.kubernetes.io/name=alertmanager
   ```
2. Verify alertmanager-config Secret:
   ```bash
   kubectl get secret alertmanager-config -n observability
   ```
3. Test notification channel credentials manually

**Alert flapping:**
1. Increase `for` duration in PrometheusRule
2. Adjust `group_wait`/`group_interval` in Alertmanager config

See `infrastructure-k8s/observability/alerting/README.md` for detailed configuration.

## Storage Management

### PVC Sizing

| Component | Default Size | Retention | Sizing Guide |
|-----------|--------------|-----------|--------------|
| Loki | 50Gi | 30 days | ~1GB per day for small cluster |
| Tempo | 30Gi | 30 days | Depends on trace volume |
| Mimir | 100Gi | 30 days | ~1-3GB per day for small cluster |
| Grafana | 10Gi | N/A | Dashboards, plugins, config |

### Adjusting Retention

**Loki** (`loki/helm-values.yaml`):
```yaml
loki:
  limits_config:
    retention_period: 720h  # 30 days
```

**Tempo** (`tempo/helm-values.yaml`):
```yaml
tempo:
  retention: 720h  # 30 days
```

**Mimir** (`mimir/helm-values.yaml`):
```yaml
mimir:
  structuredConfig:
    limits:
      compactor_blocks_retention_period: 720h  # 30 days
```

### Monitoring Storage Usage

```promql
# Loki storage
kubelet_volume_stats_used_bytes{persistentvolumeclaim=~".*loki.*"}

# Mimir storage
kubelet_volume_stats_used_bytes{persistentvolumeclaim=~".*mimir.*"}

# Tempo storage
kubelet_volume_stats_used_bytes{persistentvolumeclaim=~".*tempo.*"}
```

## Performance Tuning

### Loki

- Increase chunk sizes for write-heavy workloads
- Use stream selectors to reduce query scope
- Enable caching for frequently accessed data

### Tempo

- Configure sampling for high-volume traces
- Adjust block duration for query performance
- Use trace ID search for specific traces

### Mimir

- Tune ingester memory for cardinality
- Use recording rules for expensive queries
- Configure compactor for efficient storage

### OTel Collector

- Adjust batch processor size
- Configure memory_limiter appropriately
- Use filter processor to drop unnecessary data

## Troubleshooting

### Common Issues

#### PVCs Pending

```bash
kubectl get pvc -n observability
kubectl describe pvc <pvc-name> -n observability
# Check Proxmox CSI driver logs
kubectl logs -n csi-proxmox -l app=csi-proxmox-controller
```

#### Component Not Starting

```bash
kubectl describe pod -n observability <pod-name>
kubectl logs -n observability <pod-name>
kubectl get events -n observability --sort-by='.lastTimestamp'
```

#### No Logs in Loki

```bash
# Check OTel Collector
kubectl logs -n observability -l app.kubernetes.io/name=opentelemetry-collector --tail=100

# Verify filelog receiver
kubectl get cm otel-collector-values -n observability -o yaml | grep filelog -A 20
```

#### No Metrics in Mimir

```bash
# Check ServiceMonitors
kubectl get servicemonitor -A

# Verify endpoint accessibility
kubectl port-forward -n kube-system svc/cilium-agent 9962
curl http://localhost:9962/metrics
```

#### Grafana Data Source Errors

```bash
# From Grafana pod, test connectivity
kubectl exec -n observability deploy/grafana -- curl -s http://loki:3100/ready
kubectl exec -n observability deploy/grafana -- curl -s http://mimir-nginx:80/ready
kubectl exec -n observability deploy/grafana -- curl -s http://tempo:3100/ready
```

## Security

### Credential Management

- Grafana admin password stored in Kubernetes Secret
- Never commit passwords to git
- Consider sealed-secrets or external-secrets for production

### Network Isolation

- All components in `observability` namespace
- Use NetworkPolicies to restrict access (optional)
- Grafana is the only user-facing component

### RBAC

- ServiceAccounts created per component
- ClusterRole for OTel Collector (node/pod read access)
- Minimal permissions following least-privilege

## Backup and Recovery

### Grafana Dashboards

Dashboards are stored as ConfigMaps, so they're version-controlled in git.

Export custom dashboards:
```bash
# In Grafana UI: Dashboard → Share → Export → Save to file
```

### PVC Snapshots

Use Proxmox CSI snapshot capability:
```bash
kubectl apply -f - <<EOF
apiVersion: snapshot.storage.k8s.io/v1
kind: VolumeSnapshot
metadata:
  name: loki-backup
  namespace: observability
spec:
  source:
    persistentVolumeClaimName: storage-loki-0
EOF
```

## Verification

### End-to-End Test

1. **Deploy test log generator**:
   ```bash
   kubectl apply -f infrastructure-k8s/observability/verification/test-logging.yaml
   ```

2. **Wait for logs** (30 seconds)

3. **Query in Grafana**:
   ```logql
   {namespace="observability", app="log-generator"}
   ```

4. **Deploy test trace generator**:
   ```bash
   kubectl apply -f infrastructure-k8s/observability/verification/test-tracing.yaml
   ```

5. **Search traces** in Tempo for `service.name="test-trace-generator"`

6. **Cleanup**:
   ```bash
   kubectl delete -f infrastructure-k8s/observability/verification/test-logging.yaml
   kubectl delete -f infrastructure-k8s/observability/verification/test-tracing.yaml
   ```

### Verification Checklist

- [ ] All pods in `observability` namespace are Running
- [ ] PVCs are Bound
- [ ] Grafana accessible via port-forward
- [ ] All data sources show "Data source is working"
- [ ] Pre-installed dashboards show data
- [ ] Log queries return results
- [ ] Metric queries return results
- [ ] Trace search works

## References

- [Grafana Documentation](https://grafana.com/docs/grafana/latest/)
- [Loki Documentation](https://grafana.com/docs/loki/latest/)
- [Tempo Documentation](https://grafana.com/docs/tempo/latest/)
- [Mimir Documentation](https://grafana.com/docs/mimir/latest/)
- [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/)
- [Prometheus Operator](https://prometheus-operator.dev/)
- [LogQL Reference](https://grafana.com/docs/loki/latest/logql/)
- [PromQL Reference](https://prometheus.io/docs/prometheus/latest/querying/basics/)
- [TraceQL Reference](https://grafana.com/docs/tempo/latest/traceql/)
