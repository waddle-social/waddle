/// <reference types="astro/client" />
/// <reference path="../worker-configuration.d.ts" />

interface ImportMetaEnv {
  readonly PUBLIC_COMMIT_SHA: string;
  readonly PUBLIC_FARO_URL: string;
  readonly PUBLIC_FARO_APP_NAME: string;
  readonly PUBLIC_FARO_APP_VERSION: string;
  readonly PUBLIC_FARO_DEPLOYMENT_ENVIRONMENT: string;
  readonly PUBLIC_FARO_CLUSTER: string;
  readonly PUBLIC_FARO_NAMESPACE: string;
  readonly PUBLIC_FARO_SOURCE_ID: string;
  readonly PUBLIC_FARO_RELEASE: string;
  readonly PUBLIC_BUILD_IDENTITY_MARKER: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

// `GIPHY_API_KEY` is a Cloudflare Pages secret rather than a `wrangler.jsonc`
// var, so `wrangler types` can't see it. Augment the generated `Cloudflare.Env`
// here so the `/api/giphy` proxy route can read `env.GIPHY_API_KEY` without an
// unsafe cast. The key stays server-side; it is never passed to any island.
declare namespace Cloudflare {
  interface Env {
    GIPHY_API_KEY?: string;
  }
}
