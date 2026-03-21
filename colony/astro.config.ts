import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";

export default defineConfig({
  output: "server",
  adapter: cloudflare(),
  security: {
    // Better Auth handles origin validation for auth endpoints; Astro's generic
    // cross-site POST guard blocks OIDC token exchanges from external clients.
    checkOrigin: false,
  },
});
