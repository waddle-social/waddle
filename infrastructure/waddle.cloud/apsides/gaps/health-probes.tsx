// GAP-PROBES: /health and /ready probes and the ADR-0017 preStop sleep.
// Without them a rollout routes to unready Pods and drops in-flight streams.
// Rejected with: "Deployment containers[0] contains unsupported field livenessProbe".
import { Probe, ProbeDeployment } from "./_base.tsx";

export default (
  <Probe>
    <ProbeDeployment
      container={{
        livenessProbe: { httpGet: { path: "/health", port: 3000 }, periodSeconds: 10 },
        readinessProbe: { httpGet: { path: "/ready", port: 3000 }, periodSeconds: 10 },
        lifecycle: { preStop: { sleep: { seconds: 5 } } },
      }}
    />
  </Probe>
);
