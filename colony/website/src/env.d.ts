/// <reference types="astro/client" />

interface ImportMetaEnv {
  readonly WORKOS_API_KEY: string;
  readonly WORKOS_CLIENT_ID: string;
  readonly SESSION_SECRET: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare namespace App {
  interface Locals {
    runtime: {
      env: {
        AUTH_DB: KVNamespace;
        WORKOS_API_KEY: string;
        WORKOS_CLIENT_ID: string;
        SESSION_SECRET: string;
      };
    };
  }
}