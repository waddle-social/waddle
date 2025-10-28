// @ts-check
import { defineConfig } from 'astro/config';
import vue from '@astrojs/vue';
import cloudflare from '@astrojs/cloudflare';
import tailwindcss from '@tailwindcss/vite';

// https://astro.build/config
export default defineConfig({
  output: 'server',
  // Temporarily disabled for local dev on NixOS - uncomment for production builds
  // adapter: cloudflare(),
  server: {
    port: 4323,
    host: true,
  },
  integrations: [
    vue()
  ],
  vite: {
    plugins: [tailwindcss()],
    define: {
      'import.meta.env.APPVIEW_URL': JSON.stringify(process.env.APPVIEW_URL || 'http://localhost:8787'),
      'import.meta.env.TURNSTILE_SITE_KEY': JSON.stringify(process.env.TURNSTILE_SITE_KEY || ''),
      'import.meta.env.PUBLIC_SITE_URL': JSON.stringify(process.env.PUBLIC_SITE_URL || 'https://huddle.waddle.social')
    }
  }
});
