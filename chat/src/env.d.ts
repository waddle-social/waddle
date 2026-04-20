/// <reference types="astro/client" />
/// <reference path="../worker-configuration.d.ts" />

interface ImportMetaEnv {
  readonly PUBLIC_COMMIT_SHA: string;
  readonly PUBLIC_FARO_URL: string;
  readonly PUBLIC_FARO_APP_NAME: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
