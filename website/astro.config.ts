import { defineConfig, envField } from 'astro/config';
import tailwindcss from '@tailwindcss/vite';
import vue from '@astrojs/vue';

export default defineConfig({
  output: 'static',
  integrations: [vue()],
  vite: {
    plugins: [tailwindcss()],
  },
  server: {
    port: 4322,
    host: true,
  },
  devToolbar: {
    enabled: false
  },
  env: {
    schema: {
      PUBLIC_COLONY_BASE_URL: envField.string({
        context: 'client',
        access: 'public',
        default: 'https://colony.waddle.social',
      }),
    },
  }
});
