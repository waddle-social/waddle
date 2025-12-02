# Observability Stack (LGTM)

This directory contains the configuration for the LGTM observability stack (Loki, Grafana, Tempo, Mimir) with OpenTelemetry Collector for unified telemetry collection.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [Components](#components)
- [Manual Installation](#manual-installation)
- [Storage Configuration](#storage-configuration)
- [Dashboard Access](#dashboard-access)
- [Querying Data](#querying-data)
- [ServiceMonitor Configuration](#servicemonitor-configuration)
- [Verification](#verification)
- [Troubleshooting](#troubleshooting)
- [Security Best Practices](#security-best-practices)
- [Performance Tuning](#performance-tuning)
- [Limitations](#limitations)
- [Files in This Directory](#files-in-this-directory)
- [References](#references)

## Architecture Overview

```
Applications/Infrastructure
    │ (logs)     │ (metrics)    │ (traces)
    ▼            ▼              ▼
┌─────────────────────────────────────────────────────┐
│  OpenTelemetry Collector                            │
│  Receivers: OTLP (gRPC/HTTP), Prometheus, filelog   │
│  Processors: batch, k8sattributes, memory_limiter   │
│  Exporters: Loki, Mimir, Tempo                      │
└─────────────────────────────────────────────────────┘
    │            │              │
    ▼            ▼              ▼
┌───────┐   ┌───────┐     ┌───────┐
│ Loki  │   │ Mimir │     │ Tempo │
│(Logs) │   │(Metr.)│     │(Trace)│
└───────┘   └───────┘     └───────┘
    │            │              │
    └────────────┼──────────────┘
                 ▼
           ┌──────────┐
           │ Grafana  │
           │(Dashbd.) │
           └──────────┘
                 │
                 ▼
           User/Operator
```

**Data Flow:**
1. Applications emit logs (stdout/stderr), metrics (Prometheus endpoints), and traces (OTLP)
2. OpenTelemetry Collector receives all telemetry data
3. Collector enriches data with Kubernetes metadata (pod, namespace, labels)
4. Collector exports to appropriate backend (Loki for logs, Mimir for metrics, Tempo for traces)
5. Grafana queries all backends for unified visualization

## Prerequisites

- **Phase 6:** Cilium CNI operational (networking)
- **Phase 7:** Proxmox CSI driver installed (storage for PVCs)
- **Phase 8:** Flux GitOps setup complete
- `kubectl` configured with cluster access
- `helm` v3.x installed (for manual installation)

## Components

| Component | Description | Port | Storage |
|-----------|-------------|------|---------|
| **Prometheus Operator** | CRD provider (ServiceMonitor/PodMonitor) | - | None |
| **Grafana** | Visualization and dashboards | 3000 | 10Gi |
| **Loki** | Log aggregation (LogQL) | 3100 | 50Gi |
| **Tempo** | Distributed tracing (TraceQL) | 3100, 4317, 4318 | 30Gi |
| **Mimir** | Metrics storage (PromQL) | 8080 | 100Gi |
| **OpenTelemetry Collector** | Unified telemetry collection | 4317, 4318 | None |

## Manual Installation

### 1. Create Namespace

```bash
kubectl apply -f namespace.yaml
```

### 2. Create Grafana Admin Secret

```bash
kubectl create secret generic grafana-admin \
  --from-literal=admin-password=$(openssl rand -base64 32) \
  -n observability
```

### 3. Add Helm Repositories

```bash
helm repo add prometheus-community https://prometheus-community.github.io/helm-charts
helm repo add grafana https://grafana.github.io/helm-charts
helm repo add open-telemetry https://open-telemetry.github.io/opentelemetry-helm-charts
helm repo update
```

### 4. Install Prometheus Operator (CRDs only)

```bash
helm install prometheus-operator prometheus-community/kube-prometheus-stack \
  -n observability \
  -f prometheus-operator/helm-values.yaml \
  --wait
```

### 5. Install Loki

```bash
helm install loki grafana/loki \
  -n observability \
  -f loki/helm-values.yaml \
  --wait
```

### 6. Install Tempo

```bash
helm install tempo grafana/tempo \
  -n observability \
  -f tempo/helm-values.yaml \
  --wait
```

### 7. Install Mimir

```bash
helm install mimir grafana/mimir-distributed \
  -n observability \
  -f mimir/helm-values.yaml \
  --wait
```

### 8. Install OpenTelemetry Collector

```bash
helm install otel-collector open-telemetry/opentelemetry-collector \
  -n observability \
  -f opentelemetry/helm-values.yaml \
  --wait
```

### 9. Install Grafana

```bash
helm install grafana grafana/grafana \
  -n observability \
  -f grafana/helm-values.yaml \
  --wait
```

### 10. Deploy ServiceMonitors

```bash
kubectl apply -k servicemonitors/
```

### 11. Deploy Dashboards

```bash
kubectl apply -k dashboards/
```

## Storage Configuration

All components use Proxmox CSI for persistent storage:

| Component | StorageClass | Size | Retention |
|-----------|--------------|------|-----------|
| Loki | proxmox-csi | 50Gi | 30 days |
| Tempo | proxmox-csi | 30Gi | 30 days |
| Mimir | proxmox-csi | 100Gi | 30 days |
| Grafana | proxmox-csi | 10Gi | N/A |

Adjust sizes in respective `helm-values.yaml` files based on cluster size and log volume.

## Dashboard Access

### Port-Forward (Development)

```bash
kubectl port-forward -n observability svc/grafana 3000:80
```

Open http://localhost:3000

### Default Credentials

- **Username:** admin
- **Password:** Retrieved from Secret:
  ```bash
  kubectl get secret grafana-admin -n observability \
    -o jsonpath='{.data.admin-password}' | base64 -d
  ```

### Gateway API (Production)

Create HTTPRoute for external access (see `infrastructure-k8s/gateway/` for patterns).

## Querying Data

### Logs (Loki - LogQL)

```logql
# All logs from observability namespace
{namespace="observability"}

# Error logs from all namespaces
{} |= "error" or |= "ERROR"

# Cilium logs with rate
rate({namespace="kube-system", app="cilium"}[5m])

# PostgreSQL logs
{namespace="spicedb", postgresql="spicedb-postgres"}

# JSON log parsing
{namespace="default"} | json | level="error"
```

### Metrics (Mimir - PromQL)

```promql
# Node CPU usage
sum(rate(node_cpu_seconds_total{mode!="idle"}[5m])) by (instance)

# Pod memory usage
sum(container_memory_working_set_bytes{namespace="observability"}) by (pod)

# Cilium packet drops
rate(cilium_drop_count_total[5m])

# PostgreSQL connections
cnpg_pg_stat_activity_count

# Certificate expiry (days until expiration)
(certmanager_certificate_expiration_timestamp_seconds - time()) / 86400
```

### Traces (Tempo - TraceQL)

```traceql
# All traces
{}

# Traces with errors
{ status = error }

# Traces by service
{ resource.service.name = "my-service" }

# Slow traces (>1s duration)
{ duration > 1s }

# Traces with specific attribute
{ span.http.status_code = 500 }
```

## ServiceMonitor Configuration

ServiceMonitors/PodMonitors enable automatic metric discovery. Located in `servicemonitors/`:

| Resource | Target | Namespace |
|----------|--------|-----------|
| ServiceMonitor | Cilium | kube-system |
| ServiceMonitor | cert-manager | cert-manager |
| PodMonitor | CloudNativePG | cnpg-system |

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

## Verification

### Check All Components

```bash
# All pods running
kubectl get pods -n observability

# HelmReleases healthy
flux get helmreleases -n observability

# ServiceMonitors discovered
kubectl get servicemonitor -A

# PVCs provisioned
kubectl get pvc -n observability
```

### Test Log Ingestion

```bash
# Deploy test pod
kubectl apply -f verification/test-logging.yaml

# Wait for logs
sleep 30

# Query in Grafana Explore (Loki):
# {namespace="observability", app="log-generator"}

# Cleanup
kubectl delete -f verification/test-logging.yaml
```

### Test Trace Ingestion

```bash
# Deploy test job
kubectl apply -f verification/test-tracing.yaml

# Query in Grafana Explore (Tempo):
# Search for traces

# Cleanup
kubectl delete -f verification/test-tracing.yaml
```

### Test Metric Scraping

```bash
# In Grafana Explore (Mimir):
# up{job="cilium"}
# certmanager_certificate_ready_status
```

## Troubleshooting

### Component Not Starting

```bash
# Check pod status
kubectl describe pod -n observability <pod-name>

# Check PVC provisioning
kubectl get pvc -n observability
kubectl describe pvc -n observability <pvc-name>

# Check events
kubectl get events -n observability --sort-by='.lastTimestamp'
```

### No Logs in Loki

```bash
# Check OTel Collector logs
kubectl logs -n observability -l app.kubernetes.io/name=opentelemetry-collector

# Verify filelog receiver configuration
# Check /var/log/pods mount in collector

# Test Loki ingestion
curl -X POST http://localhost:3100/loki/api/v1/push \
  -H "Content-Type: application/json" \
  -d '{"streams":[{"stream":{"test":"true"},"values":[["'$(date +%s)000000000'","test log"]]}]}'
```

### No Metrics in Mimir

```bash
# Check ServiceMonitor discovery
kubectl get servicemonitor -A

# Verify metric endpoints
kubectl port-forward -n kube-system svc/cilium-agent 9962
curl http://localhost:9962/metrics

# Check Mimir ingestion
kubectl logs -n observability -l app.kubernetes.io/name=mimir
```

### No Traces in Tempo

```bash
# Check OTel Collector trace pipeline
kubectl logs -n observability -l app.kubernetes.io/name=opentelemetry-collector | grep trace

# Verify OTLP endpoints
kubectl port-forward -n observability svc/otel-collector 4317

# Check Tempo ingestion
kubectl logs -n observability -l app.kubernetes.io/name=tempo
```

### Grafana Login Issues

```bash
# Verify Secret exists
kubectl get secret grafana-admin -n observability

# Check Grafana logs
kubectl logs -n observability -l app.kubernetes.io/name=grafana

# Reset password
kubectl delete secret grafana-admin -n observability
kubectl create secret generic grafana-admin \
  --from-literal=admin-password=newpassword \
  -n observability
kubectl rollout restart deployment grafana -n observability
```

### OOM (Out of Memory)

```bash
# Check resource limits in helm-values.yaml
# Increase memory limits if needed
# Monitor with: kubectl top pods -n observability
```

## Security Best Practices

1. **Credentials Management:**
   - Never commit Grafana password to git
   - Use sealed-secrets or external-secrets for production
   - Rotate credentials periodically

2. **Network Policies:**
   - Restrict ingress to Grafana from specific sources
   - Allow only necessary inter-component traffic
   - Block external access to Loki/Tempo/Mimir directly

3. **RBAC:**
   - ServiceAccounts use minimal permissions
   - Grafana viewers vs editors vs admins

4. **TLS (Optional):**
   - Enable TLS between components using cert-manager
   - Configure Grafana with HTTPS

## Performance Tuning

### Loki

- Increase `chunk_idle_period` for lower write amplification
- Tune `max_chunk_age` based on log volume
- Use label selectors to reduce query scope

### Tempo

- Adjust `max_block_duration` for trace retention
- Configure sampling in OTel Collector for high-volume traces

### Mimir

- Tune `ingester.ring.replication_factor` for HA
- Configure `compactor_blocks_retention_period` for storage management
- Use recording rules for frequently-queried metrics

### OpenTelemetry Collector

- Adjust `batch` processor size for throughput vs latency
- Configure `memory_limiter` to prevent OOM
- Use `filter` processor to drop unnecessary telemetry

## Alerting

Alerting is configured via PrometheusRules and Alertmanager (Phase 15).

### Alertmanager

Alertmanager handles alert routing, grouping, and notification delivery.

```bash
# Access Alertmanager UI
kubectl port-forward -n observability svc/alertmanager 9093:9093

# Check Alertmanager pods
kubectl get pods -n observability -l app.kubernetes.io/name=alertmanager
```

### PrometheusRules

Alert rules are defined in `alerting/` directory:

| Rule File | Category | Alerts |
|-----------|----------|--------|
| `infrastructure-alerts.yaml` | Infrastructure | NodeDown, PodCrashLooping, PVCNearlyFull |
| `cilium-alerts.yaml` | Networking | CiliumAgentDown, CiliumHighPolicyDenialRate |
| `cert-manager-alerts.yaml` | Certificates | CertificateExpiringSoon, CertificateRenewalFailed |
| `cnpg-alerts.yaml` | PostgreSQL | PostgreSQLDown, PostgreSQLReplicationLag |
| `flux-alerts.yaml` | GitOps | FluxReconciliationFailure, FluxSourceNotReady |

```bash
# Check PrometheusRules
kubectl get prometheusrules -n observability

# View active alerts
kubectl port-forward -n observability svc/alertmanager 9093:9093
# Open http://localhost:9093
```

### Alert Severity Levels

| Severity | Response Time | Description |
|----------|---------------|-------------|
| critical | < 15 min | Immediate action required |
| warning | < 1 hour | Investigation needed |
| info | Next business day | Informational only |

### Notification Configuration

Configure notification channels in `alertmanager-config` Secret:

```bash
# Create Alertmanager config (see alerting/alertmanager-config-secret.example.yaml)
kubectl create secret generic alertmanager-config \
  --from-file=alertmanager.yaml \
  -n observability
```

See `alerting/README.md` for detailed configuration.

## Limitations

1. **Single Replica:** Most components deploy as single replica for simplicity. For HA, configure replicas > 1 and appropriate storage.

2. **Local Storage:** Using filesystem storage via PVCs. For production scale, consider S3-compatible object storage.

3. **Resource Requirements:** Mimir and Loki can be memory-intensive. Monitor resource usage.

## Files in This Directory

| Path | Description |
|------|-------------|
| `namespace.yaml` | Observability namespace |
| `kustomization.yaml` | Root kustomization with ConfigMapGenerator |
| `prometheus-operator/` | Prometheus Operator (CRDs only) |
| `grafana/` | Grafana visualization |
| `loki/` | Loki log aggregation |
| `tempo/` | Tempo distributed tracing |
| `mimir/` | Mimir metrics storage |
| `opentelemetry/` | OpenTelemetry Collector |
| `servicemonitors/` | ServiceMonitor/PodMonitor resources |
| `dashboards/` | Grafana dashboard ConfigMaps |
| `verification/` | Test resources for verification |

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
