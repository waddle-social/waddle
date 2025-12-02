# Observability Stack Verification Queries

This document contains example queries for verifying the observability stack is working correctly.

## Verification Steps

1. Port-forward Grafana:
   ```bash
   kubectl port-forward -n observability svc/grafana 3000:80
   ```

2. Open http://localhost:3000

3. Login with admin credentials:
   ```bash
   kubectl get secret grafana-admin -n observability \
     -o jsonpath='{.data.admin-password}' | base64 -d
   ```

4. Navigate to **Explore**

5. Select data source and run queries below

---

## Loki (LogQL) - Log Queries

### Basic Queries

```logql
# All logs from observability namespace
{namespace="observability"}

# All logs from a specific pod
{namespace="observability", pod="grafana-0"}

# Logs from kube-system namespace
{namespace="kube-system"}

# Logs containing "error" (case insensitive)
{} |~ "(?i)error"

# Logs NOT containing "info"
{} !~ "(?i)info"
```

### Filter Queries

```logql
# Error logs from all namespaces
{} |= "error" or |= "ERROR"

# Warning or error logs
{} |~ "(?i)(warn|error)"

# Specific component logs
{namespace="kube-system", app="cilium"}

# PostgreSQL logs from SpiceDB
{namespace="spicedb", cnpg.io/cluster="spicedb-postgres"}

# cert-manager logs
{namespace="cert-manager", app.kubernetes.io/name="cert-manager"}
```

### Aggregation Queries

```logql
# Log rate per namespace
sum by (namespace) (rate({} [5m]))

# Error rate per pod
sum by (pod) (rate({} |= "error" [5m]))

# Log volume over time
sum(count_over_time({namespace="observability"}[1m]))
```

### JSON Log Parsing

```logql
# Parse JSON logs and filter by level
{namespace="observability"} | json | level="error"

# Extract specific JSON fields
{namespace="default"} | json | line_format "{{.msg}}"
```

---

## Mimir (PromQL) - Metric Queries

### Cluster Health

```promql
# All up targets
up

# Node count
count(kube_node_info)

# Running pods
sum(kube_pod_status_phase{phase="Running"})

# Failed pods
sum(kube_pod_status_phase{phase="Failed"})
```

### Resource Usage

```promql
# Cluster CPU usage percentage
100 - (avg(rate(node_cpu_seconds_total{mode="idle"}[5m])) * 100)

# Node CPU usage by node
100 - (avg by (instance) (rate(node_cpu_seconds_total{mode="idle"}[5m])) * 100)

# Cluster memory usage percentage
100 * (1 - sum(node_memory_MemAvailable_bytes) / sum(node_memory_MemTotal_bytes))

# Pod memory usage
sum(container_memory_working_set_bytes{namespace="observability"}) by (pod)

# Pod CPU usage
sum(rate(container_cpu_usage_seconds_total{namespace="observability"}[5m])) by (pod)
```

### Cilium Metrics

```promql
# Cilium endpoint state
cilium_endpoint_state

# Ready endpoints
sum(cilium_endpoint_state{endpoint_state="ready"})

# Packet drop rate
rate(cilium_drop_count_total[5m])

# Drop rate by reason
sum by (reason) (rate(cilium_drop_count_total[5m]))

# Policy verdict rate
sum by (verdict) (rate(cilium_policy_verdict_total[5m]))

# Network traffic (packets/sec)
sum(rate(cilium_forward_count_total[5m]))
```

### cert-manager Metrics

```promql
# Certificate ready status
certmanager_certificate_ready_status

# Certificate expiry (seconds until expiration)
certmanager_certificate_expiration_timestamp_seconds - time()

# Certificate expiry (days)
(certmanager_certificate_expiration_timestamp_seconds - time()) / 86400

# ACME orders
certmanager_controller_sync_call_count{controller="orders"}
```

### CloudNativePG (PostgreSQL) Metrics

```promql
# PostgreSQL connections
cnpg_pg_stat_activity_count

# Active connections
cnpg_pg_stat_activity_count{state="active"}

# Database size
cnpg_pg_database_size_bytes

# Replication lag (seconds)
cnpg_pg_replication_lag

# Transaction rate
rate(cnpg_pg_stat_database_xact_commit[5m])

# Is primary
cnpg_pg_replication_is_primary
```

### Kubelet Metrics

```promql
# Running pods per node
kubelet_running_pods

# Running containers
kubelet_running_containers

# Pod start latency (p99)
histogram_quantile(0.99, rate(kubelet_pod_start_duration_seconds_bucket[5m]))
```

---

## Tempo (TraceQL) - Trace Queries

### Basic Queries

```traceql
# All traces
{}

# Traces from specific service
{ resource.service.name = "my-service" }

# Traces with errors
{ status = error }

# Traces with specific span name
{ name = "HTTP GET" }
```

### Duration Queries

```traceql
# Slow traces (>1 second)
{ duration > 1s }

# Very slow traces (>5 seconds)
{ duration > 5s }

# Fast traces (<100ms)
{ duration < 100ms }
```

### Attribute Queries

```traceql
# Traces with specific HTTP status
{ span.http.status_code = 500 }

# Traces by namespace
{ resource.k8s.namespace.name = "observability" }

# Traces by pod
{ resource.k8s.pod.name =~ "grafana.*" }

# Traces with specific attribute
{ span.test.attribute = "verification-trace" }
```

### Combined Queries

```traceql
# Slow error traces
{ status = error && duration > 500ms }

# Specific service with errors
{ resource.service.name = "my-service" && status = error }
```

---

## Verification Checklist

### Loki (Logs)
- [ ] Can query logs from observability namespace
- [ ] Can query logs from kube-system namespace
- [ ] Can filter logs by label
- [ ] Can filter logs by content
- [ ] Test log generator pod logs appear

### Mimir (Metrics)
- [ ] up metric returns targets
- [ ] Node metrics available (node_cpu_seconds_total)
- [ ] Container metrics available (container_memory_working_set_bytes)
- [ ] Cilium metrics available (cilium_endpoint_state)
- [ ] cert-manager metrics available (certmanager_certificate_ready_status)
- [ ] CNPG metrics available (cnpg_pg_stat_activity_count)

### Tempo (Traces)
- [ ] Test trace appears after running trace-generator job
- [ ] Can search traces by service name
- [ ] Can search traces by duration
- [ ] Can view trace details and spans

### Grafana
- [ ] Can login with admin credentials
- [ ] Loki data source configured and working
- [ ] Mimir data source configured and working
- [ ] Tempo data source configured and working
- [ ] Pre-installed dashboards visible
- [ ] Kubernetes Cluster Overview dashboard shows data
- [ ] Cilium dashboard shows data
- [ ] Talos dashboard shows data
- [ ] PostgreSQL dashboard shows data (if CNPG deployed)

---

## Troubleshooting

### No Logs in Loki
```bash
# Check OpenTelemetry Collector logs
kubectl logs -n observability -l app.kubernetes.io/name=opentelemetry-collector --tail=100

# Check Loki logs
kubectl logs -n observability -l app.kubernetes.io/name=loki --tail=100

# Verify filelog receiver is configured
kubectl get configmap -n observability otel-collector-values -o yaml
```

### No Metrics in Mimir
```bash
# Check ServiceMonitors exist
kubectl get servicemonitor -A

# Check Mimir logs
kubectl logs -n observability -l app.kubernetes.io/name=mimir --tail=100

# Test metric endpoint directly
kubectl port-forward -n kube-system svc/cilium-agent 9962
curl http://localhost:9962/metrics
```

### No Traces in Tempo
```bash
# Check Tempo logs
kubectl logs -n observability -l app.kubernetes.io/name=tempo --tail=100

# Check OTel Collector trace pipeline
kubectl logs -n observability -l app.kubernetes.io/name=opentelemetry-collector | grep -i trace

# Verify OTLP endpoints
kubectl port-forward -n observability svc/otel-collector-opentelemetry-collector 4317 4318
```

### Grafana Issues
```bash
# Check Grafana logs
kubectl logs -n observability -l app.kubernetes.io/name=grafana --tail=100

# Verify data sources
kubectl port-forward -n observability svc/grafana 3000:80
# Then check Configuration > Data Sources in UI

# Verify dashboards ConfigMaps
kubectl get configmap -n observability -l grafana_dashboard=1
```
