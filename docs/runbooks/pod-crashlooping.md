# Pod CrashLooping Runbook

## Alert

**Alert**: PodCrashLooping, PodOOMKilled, PodNotReady
**Severity**: Warning
**Impact**: Application unavailable or degraded, potential data loss

## Overview

A pod is restarting repeatedly, indicating application or configuration issues. This may affect service availability depending on replica count.

## Diagnosis

### 1. Check Pod Status

```bash
# List pods with restart counts
kubectl get pods -n <namespace> -o wide

# Get pod details including events
kubectl describe pod <pod-name> -n <namespace>

# Check container status
kubectl get pod <pod-name> -n <namespace> -o jsonpath='{.status.containerStatuses}' | jq
```

### 2. Check Pod Logs

```bash
# Current container logs
kubectl logs <pod-name> -n <namespace>

# Previous container logs (before crash)
kubectl logs <pod-name> -n <namespace> --previous

# Logs for specific container (multi-container pod)
kubectl logs <pod-name> -n <namespace> -c <container-name> --previous

# Follow logs
kubectl logs <pod-name> -n <namespace> -f
```

### 3. Check Resource Usage

```bash
# Current resource usage
kubectl top pod <pod-name> -n <namespace>

# Resource limits and requests
kubectl describe pod <pod-name> -n <namespace> | grep -A 10 "Limits\|Requests"

# Check resource quota in namespace
kubectl describe resourcequota -n <namespace>
```

### 4. Check Events

```bash
# Pod events
kubectl get events -n <namespace> --sort-by='.lastTimestamp' | grep <pod-name>

# All namespace events
kubectl get events -n <namespace> --sort-by='.lastTimestamp' | tail -20
```

### 5. Check Dependencies

```bash
# Check if dependent services are running
kubectl get pods -n <namespace>

# Check service endpoints
kubectl get endpoints -n <namespace>

# Check ConfigMaps and Secrets
kubectl get configmaps,secrets -n <namespace>
```

## Common Causes

1. **OOMKilled**: Container exceeded memory limits
2. **Application Error**: Bug, unhandled exception, panic
3. **Configuration Error**: Missing or invalid ConfigMap/Secret
4. **Dependency Unavailable**: Database, external service down
5. **Image Pull Error**: Image not found, registry auth failure
6. **Resource Limits**: CPU throttling, insufficient resources
7. **Liveness Probe Failure**: Health check timing issues

## Remediation

### Scenario 1: OOMKilled

The container was killed due to exceeding memory limits.

```bash
# Check if OOMKilled
kubectl describe pod <pod-name> -n <namespace> | grep -A 5 "Last State"

# Increase memory limits
kubectl edit deployment <deployment-name> -n <namespace>
# Or update the manifest and reapply
```

Example resource update:
```yaml
resources:
  requests:
    memory: "256Mi"
  limits:
    memory: "512Mi"  # Increase this
```

### Scenario 2: Application Error

```bash
# Check logs for error messages
kubectl logs <pod-name> -n <namespace> --previous

# Common patterns to look for:
# - Stack traces
# - "panic:", "fatal:", "error:"
# - Connection refused
# - File not found
```

**Action**: Fix application code and redeploy.

### Scenario 3: Configuration Error

```bash
# Check if ConfigMap/Secret exists
kubectl get configmap <configmap-name> -n <namespace>
kubectl get secret <secret-name> -n <namespace>

# Verify ConfigMap contents
kubectl describe configmap <configmap-name> -n <namespace>

# Check environment variables in pod
kubectl exec <pod-name> -n <namespace> -- env
```

**Action**: Fix ConfigMap/Secret and restart pod.

### Scenario 4: Dependency Unavailable

```bash
# Check database connectivity
kubectl exec <pod-name> -n <namespace> -- nc -zv <db-host> <db-port>

# Check DNS resolution
kubectl exec <pod-name> -n <namespace> -- nslookup <service-name>

# Check external service
kubectl exec <pod-name> -n <namespace> -- curl -v <external-url>
```

**Action**: Ensure dependencies are available before restarting pod.

### Scenario 5: Image Pull Error

```bash
# Check pod events for image pull errors
kubectl describe pod <pod-name> -n <namespace> | grep -A 5 "Events"

# Verify image exists
docker pull <image-name>:<tag>

# Check image pull secrets
kubectl get secret <secret-name> -n <namespace> -o yaml
```

**Action**: Fix image reference or registry credentials.

### Scenario 6: Liveness Probe Failure

```bash
# Check probe configuration
kubectl describe pod <pod-name> -n <namespace> | grep -A 10 "Liveness"

# Test probe endpoint manually
kubectl exec <pod-name> -n <namespace> -- curl localhost:<port>/health
```

**Action**: Adjust probe timing or fix health endpoint.

```yaml
livenessProbe:
  httpGet:
    path: /health
    port: 8080
  initialDelaySeconds: 30  # Increase if app needs more startup time
  periodSeconds: 10
  timeoutSeconds: 5
  failureThreshold: 3
```

### Emergency: Force Restart

If you need to immediately restart a crashing pod:

```bash
# Delete pod (will be recreated by controller)
kubectl delete pod <pod-name> -n <namespace>

# Scale down and up
kubectl scale deployment <deployment-name> -n <namespace> --replicas=0
kubectl scale deployment <deployment-name> -n <namespace> --replicas=<desired>

# Rollback to previous version
kubectl rollout undo deployment <deployment-name> -n <namespace>
```

## Verification

After remediation:

```bash
# Verify pod is running without restarts
kubectl get pod <pod-name> -n <namespace> -w

# Check logs for healthy operation
kubectl logs <pod-name> -n <namespace> -f

# Verify application is responding
kubectl exec <pod-name> -n <namespace> -- curl localhost:<port>/health
```

## Escalation

- **15 minutes**: If root cause unclear, escalate to Application Team
- **30 minutes**: If impacting production, consider rollback
- **1 hour**: Escalate to Development Lead for code-level issues

## Related Documentation

- [Node Down Runbook](node-down.md) - If node issues are causing pod failures
- [PostgreSQL Down Runbook](postgresql-down.md) - For database dependency issues
- [Kubernetes Debugging](https://kubernetes.io/docs/tasks/debug/)
