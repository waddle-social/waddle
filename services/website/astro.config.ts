// @ts-check
import { defineConfig, envField } from 'astro/config';
import cloudflare from '@astrojs/cloudflare';

const site = (): string => {
  if (import.meta.env.CF_PAGES_URL) {
    return import.meta.env.CF_PAGES_URL;
  }

  if (import.meta.env.DEV) {
    return "http://localhost:4321";
  }

  return "https://waddle.social";
};

// https://astro.build/config
export default defineConfig({
  site: site(),
  output: 'server',
  adapter: cloudflare({
    platformProxy: {
      enabled: true,
    },
  }),
  env: {
    schema: {
      WORKOS_CLIENT_ID: envField.string({
        context: 'server',
        access: 'secret',
      }),
      WORKOS_API_KEY: envField.string({
        context: 'server',
        access: 'secret',
      }),
      WORKOS_COOKIE_SECRET: envField.string({
        context: 'server',
        access: 'secret',
      }),
    },
  }
});
