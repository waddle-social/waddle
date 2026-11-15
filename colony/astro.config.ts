import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import panda from "@pandacss/vite";
import { waddleFonts } from "@waddle/design/astro-fonts";

export default defineConfig({
  compressHTML: true,
  output: "server",
  adapter: cloudflare(),
  fonts: waddleFonts,
  vite: { plugins: [panda()] },
  security: {
    // Better Auth handles origin validation for auth endpoints; Astro's generic
    // cross-site POST guard blocks OIDC token exchanges from external clients.
    checkOrigin: false,
  },
});
