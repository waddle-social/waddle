import { defineConfig, fontProviders } from "astro/config";
import cloudflare from "@astrojs/cloudflare";

export default defineConfig({
  output: "server",
  adapter: cloudflare(),
  fonts: [
    {
      provider: fontProviders.google(),
      name: "Inter",
      cssVariable: "--font-inter",
      weights: [400, 500, 600, 700, 800],
      styles: ["normal"],
      subsets: ["latin"],
    },
    {
      provider: fontProviders.google(),
      name: "Fredoka",
      cssVariable: "--font-fredoka",
      weights: [400, 500, 600, 700],
      styles: ["normal"],
      subsets: ["latin"],
    },
  ],
  security: {
    // Better Auth handles origin validation for auth endpoints; Astro's generic
    // cross-site POST guard blocks OIDC token exchanges from external clients.
    checkOrigin: false,
  },
});
