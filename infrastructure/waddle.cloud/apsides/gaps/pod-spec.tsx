// GAP-POD-SPEC: terminationGracePeriodSeconds (drain budget), fsGroup and pod
// anti-affinity.
// Rejected with: "Deployment.spec.template.spec contains unsupported field terminationGracePeriodSeconds".
import { labels, Probe, ProbeDeployment } from "./_base.tsx";

export default (
  <Probe>
    <ProbeDeployment
      pod={{
        terminationGracePeriodSeconds: 45,
        securityContext: { fsGroup: 1000 },
        affinity: {
          podAntiAffinity: {
            preferredDuringSchedulingIgnoredDuringExecution: [{
              weight: 50,
              podAffinityTerm: { labelSelector: { matchLabels: labels }, topologyKey: "kubernetes.io/hostname" },
            }],
          },
        },
      }}
    />
  </Probe>
);
