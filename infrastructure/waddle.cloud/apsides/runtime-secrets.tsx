import { ExternalSecret } from "./.aps/crds/external-secret.ts";
import { namespace, secretStore } from "./site.ts";

const serverRuntime = (secretKey: string, property: string) => ({
  secretKey,
  remoteRef: { key: "server-runtime-production", property },
});

const livekit = (secretKey: string, property: string) => ({
  secretKey,
  remoteRef: { key: "livekit-sfu", property },
});

// The Flux bundle adds a `WADDLE_RUNTIME_SECRETS_CHECKSUM` key and the
// `reconcile.fluxcd.io/watch` label so Flux re-renders the HelmRelease when a
// value rotates. The waddle-server Deployment observes these Secrets with
// `rolloutOn` instead, so neither is declared here.
export const RuntimeSecrets = () => (
  <>
    <ExternalSecret
      metadata={{ name: "waddle-runtime-secrets", namespace }}
      spec={{
        refreshInterval: "1h",
        secretStoreRef: secretStore,
        target: { name: "waddle-runtime-secrets", creationPolicy: "Owner" },
        data: [
          serverRuntime("WADDLE_SESSION_KEY", "session-key"),
          serverRuntime("WADDLE_OCCUPANT_ID_SECRET", "occupant-id-secret"),
          serverRuntime("WADDLE_S3_ACCESS_KEY_ID", "r2-access-key-id"),
          serverRuntime("WADDLE_S3_SECRET_ACCESS_KEY", "r2-secret-access-key"),
          serverRuntime("WADDLE_SPICEDB_PRESHARED_KEY", "spicedb-preshared-key"),
          serverRuntime("WADDLE_PROVIDER_GITHUB_WEBHOOK_SECRET", "github-app-webhook-secret"),
        ],
      }}
    />
    <ExternalSecret
      metadata={{ name: "waddle-openrouter", namespace }}
      spec={{
        refreshInterval: "1h",
        secretStoreRef: secretStore,
        target: {
          name: "waddle-openrouter",
          creationPolicy: "Owner",
          template: { engineVersion: "v2", data: { api_key: "{{ .apiKey }}" } },
        },
        data: [serverRuntime("apiKey", "openrouter-api-key")],
      }}
    />
    <ExternalSecret
      metadata={{ name: "waddle-livekit-config", namespace }}
      spec={{
        refreshInterval: "1h",
        secretStoreRef: secretStore,
        target: {
          name: "waddle-livekit-config",
          creationPolicy: "Owner",
          template: {
            engineVersion: "v2",
            mergePolicy: "Merge",
            // Names match `waddle-sfu::SfuConfig::from_env`. `LIVEKIT_WS_URL`
            // must be the hostname browsers reach through the livekit-sfu
            // HTTPRoute; see ../gitops/waddle-server/livekit-external-secret.yaml.
            data: {
              LIVEKIT_WS_URL: "wss://sfu.waddle.social",
              LIVEKIT_TURN_HOST: "turn.waddle.social",
              LIVEKIT_TURN_TLS_PORT: "443",
              LIVEKIT_TURN_UDP_PORT: "30478",
            },
          },
        },
        data: [
          livekit("LIVEKIT_API_KEY", "api-key"),
          livekit("LIVEKIT_API_SECRET", "api-secret"),
          // Dedicated webhook secret, never the room JWT `api-secret`.
          livekit("LIVEKIT_WEBHOOK_SECRET", "webhook-secret"),
          livekit("LIVEKIT_TURN_SHARED_SECRET", "turn-shared-secret"),
        ],
      }}
    />
  </>
);
