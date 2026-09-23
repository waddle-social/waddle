import { defineNamespacedResource } from "@apsides/kubernetes";

// Apsides models only ConfigMap, Service and Deployment as builtins, but it
// accepts any namespaced resource outside the core API group through the
// custom-resource path. These descriptors use that path for two built-in
// kinds the chart renders. `aps crds fetch` imports only CRDs, so the specs
// are untyped, and schemaSha256 binds the upstream OpenAPI document for the
// cluster's Kubernetes version instead of a CRD:
// https://raw.githubusercontent.com/kubernetes/kubernetes/v1.35.0/api/openapi-spec/v3/apis__policy__v1_openapi.json
// https://raw.githubusercontent.com/kubernetes/kubernetes/v1.35.0/api/openapi-spec/v3/apis__networking.k8s.io__v1_openapi.json
type UntypedSpec = Record<string, unknown>;

export const PodDisruptionBudget = defineNamespacedResource<UntypedSpec>({
  apiVersion: "policy/v1",
  kind: "PodDisruptionBudget",
  apiResource: {
    plural: "poddisruptionbudgets",
    scope: "Namespaced",
    schemaSha256: "sha256:166c6c720025c3b5a03e4cd4698a9dd876de383df07a9c0955cff8da833d51d5",
  },
});

export const NetworkPolicy = defineNamespacedResource<UntypedSpec>({
  apiVersion: "networking.k8s.io/v1",
  kind: "NetworkPolicy",
  apiResource: {
    plural: "networkpolicies",
    scope: "Namespaced",
    schemaSha256: "sha256:6b974984cb19f1ef1832fdfa84b67ce600eff1db354cdb49db0edb1cf87e234f",
  },
});
