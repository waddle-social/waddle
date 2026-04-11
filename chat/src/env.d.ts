/// <reference path="../.astro/types.d.ts" />

declare module "cloudflare:workers" {
  export const env: {
    SERVER_BASE_URL: string;
    GIPHY_API_KEY?: string;
  };
}
