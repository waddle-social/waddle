// @ts-check
import { defineConfig } from 'astro/config';
import tailwindcss from '@tailwindcss/vite';
import cloudflare from '@astrojs/cloudflare';

// https://astro.build/config
export default defineConfig({
  output: 'server',
  adapter: cloudflare({
    mode: 'directory',
    platformProxy: {
      enabled: true,
    },
  }),
  vite: {
    plugins: [tailwindcss()],
    ssr: {
      external: [
        'crypto',
        'util',
        'util/types',
        'string_decoder',
        'node:crypto',
        'node:util',
        'node:util/types',
        'node:dns',
        'node:dns/promises',
        'node:http',
        'node:http2',
        'node:assert',
        'node:async_hooks',
        'node:stream',
        'node:buffer',
        'node:events',
        'node:url',
        'node:querystring',
        'node:net',
        'node:tls',
        'node:zlib',
        'node:diagnostics_channel',
        'node:perf_hooks',
        'node:worker_threads',
        'node:console'
      ],
      noExternal: ['@atproto/oauth-client-node', '@atproto/api'],
    },
  },
});
