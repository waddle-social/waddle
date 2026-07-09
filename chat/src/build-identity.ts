import type { FaroBuildIdentityScope } from "@/build-identity-contract";

export const webBuildIdentity = Object.freeze({
  commitSha: import.meta.env.PUBLIC_COMMIT_SHA,
  marker: import.meta.env.PUBLIC_BUILD_IDENTITY_MARKER,
  faroRelease: import.meta.env.PUBLIC_FARO_RELEASE,
  faroScope: Object.freeze<FaroBuildIdentityScope>({
    deploymentEnvironment: import.meta.env.PUBLIC_FARO_DEPLOYMENT_ENVIRONMENT,
    cluster: import.meta.env.PUBLIC_FARO_CLUSTER,
    namespace: import.meta.env.PUBLIC_FARO_NAMESPACE,
    sourceId: import.meta.env.PUBLIC_FARO_SOURCE_ID,
    release: import.meta.env.PUBLIC_FARO_RELEASE,
  }),
});
