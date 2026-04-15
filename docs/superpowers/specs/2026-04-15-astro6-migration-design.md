# Astro 6 Migration with Persistent WebSocket

## Context

The chat app was migrated from Astro (SSR + islands) to a Vite SPA. However, with plans to add dashboards, forums, and games, a monolithic SPA will become unwieldy. Astro's islands architecture is a better fit — each page loads only what it needs, file-based routing keeps things organized, and SSR capabilities remain available.

The challenge is keeping the XMPP WebSocket alive across page navigations. Astro View Transitions with `transition:persist` solve this — a persisted Vue island retains its DOM, state, and WebSocket connection during soft navigations.

This spec covers reverting to Astro 6 with Cloudflare, adding View Transitions, and extracting an XMPP provider pattern for connection persistence. All styling changes from the SPA migration are preserved.

## Architecture

### Build & Deploy Stack

- **Astro 6** (`astro@^6`) — framework
- **@astrojs/cloudflare@^13** — SSR adapter for Cloudflare Workers
- **@astrojs/vue@^5** — Vue 3 island integration
- **@tailwindcss/vite** — Tailwind CSS via Vite plugin (Astro uses Vite under the hood)
- **Cloudflare Workers** — deployment target with server-side env access

### File Structure

```
chat/
├── astro.config.mjs              # Restored with View Transitions
├── wrangler.jsonc                 # Restored Cloudflare config
├── package.json                   # Astro deps restored, SPA deps removed
├── tsconfig.json
└── src/
    ├── layouts/
    │   └── AppLayout.astro        # NEW: shared layout with persistent XMPP island
    ├── pages/
    │   ├── index.astro            # Landing/redirect
    │   └── chat/
    │       └── [...path].astro    # Chat interface
    ├── components/
    │   ├── XmppProvider.vue       # NEW: persistent connection manager
    │   ├── ChatApp.vue            # Modified: injects XMPP client instead of owning it
    │   ├── chat/                  # Existing chat components (unchanged)
    │   ├── modals/                # Existing modal components (unchanged)
    │   └── ui/                    # Existing UI primitives (unchanged)
    ├── composables/               # Existing composables (minimal changes)
    ├── lib/                       # Existing XMPP lib (unchanged)
    ├── styles/
    │   └── global.css             # Preserved with all styling updates
    └── env.d.ts                   # Astro env types
```

### Files to Delete (SPA artifacts)

- `chat/index.html`
- `chat/src/main.ts`
- `chat/vite.config.ts`
- `chat/worker.ts`

## View Transitions + Persistent WebSocket

### AppLayout.astro

The shared layout wraps all pages:

```astro
---
import { ViewTransitions } from 'astro:transitions';
import { env } from 'cloudflare:workers';
import XmppProvider from '@/components/XmppProvider.vue';
import '@/styles/global.css';

const serverBaseUrl = env.SERVER_BASE_URL;
---
<html lang="en" class="dark">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Waddle</title>
    <ViewTransitions />
  </head>
  <body class="bg-background text-foreground font-sans antialiased">
    <XmppProvider client:load transition:persist serverBaseUrl={serverBaseUrl} />
    <slot />
  </body>
</html>
```

Key points:
- `<ViewTransitions />` enables soft navigation between pages
- `XmppProvider` with `client:load transition:persist` keeps the XMPP WebSocket alive across navigations
- `global.css` imported here (not in main.ts)

### XmppProvider.vue (NEW)

A persistent Vue component that manages the XMPP connection lifecycle:

- Creates and holds the `BrowserXmppClient` instance
- Manages auth state (bootstrap, login, logout)
- Uses Vue's `provide()` to expose the client and auth state to descendant components
- Renders minimally (possibly just a `<slot>` or nothing visible — the real UI lives in page-specific islands)

Accepts `serverBaseUrl` as a prop (passed from the Astro page which reads it from Cloudflare env).

Exposes via the shared connection store:
- `xmppClient` — the `BrowserXmppClient` instance
- `session` — current auth session
- `appState` — 'loading' | 'ready' | 'signed-out' | 'error'
- `login()` / `logout()` — auth actions

### ChatApp.vue (Modified)

Changes:
- Remove XMPP client creation and auth bootstrap (moved to XmppProvider)
- Use `inject()` to get `xmppClient`, `session`, `appState`, `login`, `logout`
- Keep all existing UI logic, composable usage, and component tree
- The component becomes a "page shell" that renders the chat UI using an injected connection

### Cross-Island Communication

Astro islands are independent by default — `provide()`/`inject()` only works within the same Vue app instance. Since `XmppProvider` and `ChatApp` are separate islands, we need a different mechanism.

**Approach: Shared module-level state**

Create a shared reactive store (`src/lib/xmpp/connection-store.ts`) using Vue's `reactive()` at module scope:

```typescript
import { reactive, type ShallowRef, shallowRef } from 'vue';
import type { BrowserXmppClient } from './client';

export const connectionStore = reactive({
  client: shallowRef<BrowserXmppClient | null>(null),
  appState: 'loading' as 'loading' | 'ready' | 'signed-out' | 'error',
  session: null as SessionData | null,
});
```

- `XmppProvider` writes to this store
- `ChatApp` (and future dashboard/forum components) reads from it
- Works across islands because they share the same JS module in the browser bundle

## Dependency Changes

### Add back
- `astro@^6`
- `@astrojs/cloudflare@^13`
- `@astrojs/vue@^5`
- `@astrojs/check` (dev)

### Remove
- `@vitejs/plugin-vue` (Astro handles Vue compilation)

### Keep (from SPA migration)
- `@fontsource-variable/inter`
- `@fontsource-variable/jetbrains-mono`
- All existing deps (vue, stanza, tailwindcss, lucide-vue-next, etc.)

## Package.json Scripts

```json
{
  "dev": "astro dev",
  "build": "astro check && astro build",
  "preview": "astro preview",
  "astro": "astro",
  "deploy": "bunx wrangler pages deploy",
  "generate-types": "bunx wrangler types"
}
```

## Wrangler Config

Restore to point at Astro's Cloudflare entrypoint:

```jsonc
{
  "name": "waddle-chat",
  "main": "@astrojs/cloudflare/entrypoints/server",
  "compatibility_date": "2026-03-17",
  "compatibility_flags": ["nodejs_compat"],
  "assets": {
    "binding": "ASSETS",
    "run_worker_first": true
  },
  "vars": {
    "SERVER_BASE_URL": "https://xmpp.waddle.social"
  }
}
```

## Server-Side Env Injection

Pages read from Cloudflare env at request time (Astro 6 pattern):

```astro
---
import { env } from 'cloudflare:workers';
const serverBaseUrl = env.SERVER_BASE_URL;
---
<ChatApp client:load serverBaseUrl={serverBaseUrl} />
```

This keeps secrets out of JS bundles.

## Styling

All styling changes from the SPA migration are preserved exactly:

- Font imports: `@fontsource-variable/inter`, `@fontsource-variable/jetbrains-mono`
- Linear-inspired color scheme (light + dark themes)
- Custom scrollbar styles (thin 6px, themed)
- Animations: `message-in`, `typing-dot`, `pulse-glow`, `fade-in`, `slide-up`
- Message body formatting (code blocks, blockquotes, links)
- Font feature settings for Inter

No changes to `global.css` content — only the import location moves from `main.ts` to `AppLayout.astro`.

## Astro Config

```javascript
import { defineConfig } from "astro/config";
import vue from "@astrojs/vue";
import tailwindcss from "@tailwindcss/vite";
import cloudflare from "@astrojs/cloudflare";

export default defineConfig({
  output: "server",
  adapter: cloudflare(),
  integrations: [vue()],
  vite: {
    plugins: [tailwindcss()],
    resolve: {
      alias: {
        events: "events",
      },
    },
    optimizeDeps: {
      include: ["events", "stanza"],
    },
  },
  server: {
    port: 4321,
  },
});
```

## env.d.ts

Revert to Astro environment types:

```typescript
/// <reference types="astro/client" />
```

## Verification

1. `bun install` — deps install cleanly
2. `bun run dev` — Astro dev server starts on :4321
3. Navigate to `/chat` — ChatApp renders, XMPP connects
4. Navigate to `/` then back to `/chat` — WebSocket stays connected (check browser devtools Network tab for no new WS connection)
5. `bun run build` — builds without errors
6. `astro check` — no type errors
7. Deploy to Cloudflare — env vars injected server-side, chat works
