# Quickstart: Waddle Chat Clickdummy (Astro + Vue + Ark UI + Tailwind)

Absolute paths assume this repo at: `/Users/icepuma/development/waddle`

## 1) Create the app scaffold

```bash
cd /Users/icepuma/development/waddle
bunx --bun create astro@latest waddle/app -- --template minimal --no
cd waddle/app
bun add -D typescript @types/node
bun add astro @astrojs/vue @astrojs/cloudflare vue tailwindcss @tailwindcss/vite @ark-ui/vue
```

## 2) Configure Astro + Vue + Tailwind

- `waddle/app/astro.config.mjs`
  - Add `vue()` integration; set `vite.plugins = [tailwind()]`; set `output: 'static'`.
- `waddle/app/tailwind.config.mjs`
  - Use content glob: `['./src/**/*.{astro,html,js,jsx,md,mdx,ts,tsx,vue}']`.
- `waddle/app/src/styles/global.css`
  - Import Tailwind and define tokens.

## 3) Import assets and translate components

- Copy assets from the downloaded source:
  - Source: `/Users/icepuma/Downloads/waddle-chat/public/*`
  - Dest:   `/Users/icepuma/development/waddle/waddle/app/public/`
- Convert presentational components from the downloaded Next.js project into Vue SFCs under `src/components`.
- Map routes from `/Users/icepuma/Downloads/waddle-chat/app` to `waddle/app/src/pages`.

## 4) Add mock state and pages

- Create `src/types` using entities from `specs/001-migrate-bun-workspaces/data-model.md`.
- Implement `src/pages/index.astro` (login stub) and `src/pages/chat.astro` that mounts a Vue chat shell.
- Compose UI with Ark UI primitives and Tailwind classes.

## 5) Run

```bash
cd /Users/icepuma/development/waddle/waddle/app
bun dev
```

## 6) Test (smoke)

```bash
# from repo root
bun test --filter './waddle/*'
```

## Notes
- This is a clickdummy only; no backend or networking.
- When backend scope is defined, wire to Cloudflare Workers and update contracts in `specs/001-migrate-bun-workspaces/contracts/openapi.yaml`.
