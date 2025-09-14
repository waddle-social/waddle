/// <reference types="astro/client" />

type D1Database = import('@cloudflare/workers-types').D1Database;

interface SecretStore {
  get(): Promise<string>;
}

interface ImportMetaEnv {
  readonly ATPROTO_PRIVATE_KEY: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare namespace App {
  interface Locals {
    runtime: {
      env: {
        DB: D1Database;
        ATPROTO_PRIVATE_KEY: SecretStore;
      };
    };
  }
}