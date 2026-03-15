/// <reference path="../.astro/types.d.ts" />

declare namespace Cloudflare {
  interface Env {
    BETTER_AUTH_SECRET: string;
    GITHUB_CLIENT_SECRET: string;
  }
}

declare namespace App {
  interface Locals {
    user: import("better-auth").User | null;
    session: import("better-auth").Session | null;
  }
}
