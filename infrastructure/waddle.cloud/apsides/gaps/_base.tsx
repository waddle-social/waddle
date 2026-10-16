import { Controller, Deployment, type ApsidesNode, type DeploymentDefinition } from "@apsides/kubernetes";

// Shared scaffolding for the gap probes. Each probe declares one Flux-era
// shape from ../../gitops that the waddle program had to drop. check.sh
// requires every probe to fail and control.tsx to compile.
export const namespace = "waddle";
export const labels = { app: "probe" };

export const Probe = ({ children }: { readonly children?: ApsidesNode }) => (
  <Controller
    id="gap-probe"
    namespace={namespace}
    controlNamespace="apsides-system"
    clusterBinding="waddle-production"
    policyRef="platform/gap-probe"
    enforcement="api-bound"
    budgets={{ maxResources: 8, maxInputs: 8, maxReactions: 8, maxObjectBytes: 262_144, maxQueueDepth: 8, maxConcurrentRequests: 1 }}
  >
    {children}
  </Controller>
);

type PodSpec = Record<string, unknown>;
type Container = Record<string, unknown>;

// A Deployment the SDK accepts, extended with fields it does not model. The
// cast lets the SDK and compiler, not the TypeScript types, decide.
export const ProbeDeployment = (
  { spec = {}, pod = {}, container = {} }: { readonly spec?: PodSpec; readonly pod?: PodSpec; readonly container?: Container },
) => (
  <Deployment
    metadata={{ name: "probe", namespace }}
    spec={{
      replicas: 1,
      selector: { matchLabels: labels },
      template: {
        metadata: { labels },
        spec: {
          automountServiceAccountToken: false,
          containers: [{ name: "probe", image: "ghcr.io/waddle-social/waddle:main", ...container }],
          ...pod,
        },
      },
      ...spec,
    } as DeploymentDefinition["spec"]}
  />
);
