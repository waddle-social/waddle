/// <reference types="astro/client" />

interface ImportMetaEnv {
  readonly APPVIEW_URL: string;
  readonly TURNSTILE_SITE_KEY: string;
  readonly PUBLIC_SITE_URL: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}