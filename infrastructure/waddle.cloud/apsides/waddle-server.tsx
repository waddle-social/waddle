import { ConfigMap, Deployment, Service, useResourceRef, useSecret } from "@apsides/kubernetes";
import { NetworkPolicy, PodDisruptionBudget } from "./builtin-resources.ts";
import { extensions, gitSha, serverImage } from "./release.ts";
import { namespace, xmppHost } from "./site.ts";

// Objects the waddle-server chart renders for the production values in
// ../gitops/waddle-server/helmrelease.yaml (chart 0.4.5). Each `GAP-*` marks
// a chart field Apsides cannot express yet; ASSESSMENT.md explains the impact
// and gaps/ holds a failing probe per gap.
const labels = { "app.kubernetes.io/name": "waddle-server", "app.kubernetes.io/instance": "waddle-server" };

const authProviders = JSON.stringify([{
  id: "colony",
  display_name: "Colony",
  kind: "oidc",
  dynamic_client_registration: true,
  client_id: "",
  token_endpoint_auth_method: "none",
  require_dpop: true,
  issuer: "https://colony.waddle.social",
  scopes: ["openid", "profile", "email"],
  subject_claim: "sub",
  username_claim: "preferred_username",
  email_claim: "email",
}]);

const config = {
  WADDLE_MODE: "homeserver",
  WADDLE_BASE_URL: `https://${xmppHost}`,
  WADDLE_DB_DRIVER: "postgres",
  WADDLE_DEPLOYMENT_UUID: "b90424f8-22ca-40bb-a24e-551d171c7b84",
  WADDLE_DB_POOL_SIZE: "10",
  WADDLE_DB_CONTROL_PLANE_POOL_SIZE: "4",
  WADDLE_XMPP_ENABLED: "true",
  WADDLE_XMPP_DOMAIN: "waddle.social",
  WADDLE_XMPP_PUBLIC_WEBSOCKET_URL: `wss://${xmppHost}/ws`,
  WADDLE_NATIVE_AUTH_ENABLED: "true",
  WADDLE_REGISTRATION_ENABLED: "false",
  WADDLE_EXTENSIONS_JSON: JSON.stringify(extensions),
  WADDLE_DRAIN_TIMEOUT_SECS: "30",
  WADDLE_CLUSTERING_ENABLED: "true",
  WADDLE_CLUSTERING_LISTEN_ADDRS: "/ip4/0.0.0.0/tcp/7900",
  // GAP-HEADLESS-SERVICE: the chart points peers at a headless Service that
  // publishes not-ready addresses. Only a ClusterIP Service can be declared,
  // so bootstrap dials one load-balanced virtual IP instead of every Pod.
  WADDLE_CLUSTERING_BOOTSTRAP_PEERS: "waddle-server-swarm:7900",
  WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS: "5000",
  WADDLE_SERVER_OWNER_LOCALPARTS: "rawkode,icepuma,randax",
  RUST_LOG: "info",
  WADDLE_CORS_ORIGINS: "https://waddle.chat,http://localhost:4321",
  // Production talks plaintext gRPC to the in-cluster SpiceDB (the Flux values
  // set the same endpoint with WADDLE_SPICEDB_INSECURE); TLS there is separate work.
  WADDLE_SPICEDB_ENDPOINT: "http://spicedb:50051", // NOSONAR(typescript:S5332)
  WADDLE_SPICEDB_INSECURE: "true",
  OTEL_EXPORTER_OTLP_ENDPOINT: "http://grafana-alloy.grafana-alloy.svc.cluster.local:4317",
  OTEL_EXPORTER_OTLP_PROTOCOL: "grpc",
  OTEL_SERVICE_NAME: "waddle-server",
  OTEL_RESOURCE_ATTRIBUTES: "deployment.environment=production",
  OTEL_TRACES_SAMPLER: "parentbased_traceidratio",
  OTEL_TRACES_SAMPLER_ARG: "1.0",
  WADDLE_S3_ENDPOINT: "https://f90cc3950ab5b356ec869fe64c867ea7.r2.cloudflarestorage.com",
  WADDLE_S3_BUCKET: "waddle-social-files",
  // The chart keeps this in a Secret although it holds no credential; the
  // chart's database URLs in that Secret are overridden by `env` below.
  WADDLE_AUTH_PROVIDERS_JSON: authProviders,
  WADDLE_UPLOAD_DIR: "/var/lib/waddle/uploads",
  WADDLE_XMPP_TLS_CERT: "/etc/waddle/tls/tls.crt",
  WADDLE_XMPP_TLS_KEY: "/etc/waddle/tls/tls.key",
  WADDLE_PROVIDER_GITHUB_WEBHOOK_EVENT_HEADER: "x-github-event",
  WADDLE_PROVIDER_GITHUB_WEBHOOK_DELIVERY_HEADER: "x-github-delivery",
  WADDLE_PROVIDER_GITHUB_WEBHOOK_SIGNATURE_HEADER: "x-hub-signature-256",
  WADDLE_PROVIDER_GITHUB_WEBHOOK_SIGNATURE_PREFIX: "sha256=",
};

const databaseUrl = (name: string) => ({
  name,
  valueFrom: { secretKeyRef: { name: "postgresql-app", key: "uri" } },
});

export const WaddleServer = () => {
  // Explicit ids: the SDK derives rollout ids as
  // `rollout-<input id>-<deployment id>`, which exceeds its own 63-character
  // limit with these object names.
  const serverConfig = useResourceRef("ConfigMap");
  const runtime = useSecret({ id: "runtime-secrets", name: "waddle-runtime-secrets" });
  const keypool = useSecret({ id: "clustering-keypool", name: "waddle-clustering-keypool" });
  const livekit = useSecret({ id: "livekit-config", name: "waddle-livekit-config" });
  const database = useSecret({ id: "database", name: "postgresql-app" });

  return (
    <>
      <ConfigMap id="server-config" ref={serverConfig} metadata={{ name: "waddle-server-config", namespace, labels }} data={config} />
      <Service
        metadata={{ name: "waddle-server", namespace, labels }}
        spec={{ type: "ClusterIP", selector: labels, ports: [{ name: "http", port: 3000, targetPort: "http", protocol: "TCP" }] }}
      />
      <Service
        metadata={{ name: "waddle-server-swarm", namespace, labels }}
        spec={{ type: "ClusterIP", selector: labels, ports: [{ name: "swarm", port: 7900, targetPort: "swarm", protocol: "TCP" }] }}
      />
      {/* GAP-SERVICE-ACCOUNT: the chart's ServiceAccount has no representation. */}
      <PodDisruptionBudget
        metadata={{ name: "waddle-server", namespace, labels }}
        spec={{ maxUnavailable: 1, selector: { matchLabels: labels } }}
      />
      <NetworkPolicy
        metadata={{ name: "waddle-server-swarm", namespace, labels }}
        spec={{
          podSelector: { matchLabels: labels },
          policyTypes: ["Ingress"],
          ingress: [
            { ports: [{ protocol: "TCP", port: 3000 }] },
            { from: [{ podSelector: { matchLabels: labels } }], ports: [{ protocol: "TCP", port: 7900 }] },
          ],
        }}
      />
      <Deployment
        id="server"
        metadata={{ name: "waddle-server", namespace, labels }}
        rolloutOn={[serverConfig.data, runtime.data, keypool.data, livekit.data, database.data]}
        spec={{
          replicas: 2,
          // GAP-ROLLOUT-STRATEGY: maxSurge 1 / maxUnavailable 0, and the
          // one-shot Recreate cutovers, cannot be declared.
          selector: { matchLabels: labels },
          template: {
            metadata: { labels },
            spec: {
              // GAP-POD-SPEC: terminationGracePeriodSeconds, fsGroup, volumes
              // and pod anti-affinity cannot be declared.
              automountServiceAccountToken: false,
              containers: [{
                name: "waddle-server",
                image: serverImage,
                imagePullPolicy: "Always",
                // GAP-CONTAINER-SECURITY: capabilities, runAsUser/runAsGroup,
                // runAsNonRoot and allowPrivilegeEscalation are all rejected
                // by the SDK. GAP-PROBES: liveness/readiness probes and the
                // preStop sleep cannot be declared. GAP-VOLUMES: the OpenRouter
                // key file, the data emptyDir and the XMPP TLS certificate are
                // not mounted (the server never reads WADDLE_XMPP_TLS_*, so the
                // last one is inert chart config).
                ports: [
                  { name: "http", containerPort: 3000, protocol: "TCP" },
                  { name: "swarm", containerPort: 7900, protocol: "TCP" },
                ],
                envFrom: [
                  { configMapRef: { name: "waddle-server-config" } },
                  { secretRef: { name: "waddle-runtime-secrets" } },
                  { secretRef: { name: "waddle-clustering-keypool" } },
                  { secretRef: { name: "waddle-livekit-config" } },
                ],
                // GAP-DOWNWARD-API: WADDLE_CLUSTERING_POD_TEMPLATE_HASH needs a
                // fieldRef to the pod-template-hash label; without it the
                // rollout-aware claim backoff has no generation to compare.
                env: [
                  databaseUrl("WADDLE_DATABASE_URL"),
                  databaseUrl("WADDLE_XMPP_MAM_DATABASE_URL"),
                  databaseUrl("WADDLE_XMPP_INBOX_DATABASE_URL"),
                  databaseUrl("WADDLE_XMPP_PUBSUB_DATABASE_URL"),
                  ...(gitSha === undefined ? [] : [{ name: "WADDLE_GIT_SHA", value: gitSha }]),
                ],
                resources: {
                  requests: { cpu: "100m", memory: "512Mi" },
                  limits: { cpu: "1", memory: "4Gi" },
                },
              }],
            },
          },
        }}
      />
    </>
  );
};
