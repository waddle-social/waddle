/**
 * Panda CSS 2 for Waddle chat, on the shared Polar Huddle preset
 * (`@waddle/design`). Tokens, semantic tokens, text styles, keyframes and
 * the `wd-*` recipes all come from the preset; chat adds no recipes of its
 * own. Chat-only part classes (menu, card and toast anatomies) are plain
 * `css()` objects in `src/ui/*-classes.ts`.
 *
 * Integration: `@pandacss/vite` in astro.config.mjs. It appends the
 * generated CSS to the stylesheet that declares the Panda layers
 * (src/styles/global.css). `panda codegen` still runs before dev, build,
 * test and lint so `styled-system/` exists for type checking and Knip.
 *
 * Cascade: layers are prefixed `pd_*`; global.css fixes their order around
 * the legacy `base` (body) and `components` (`.chat-*` hooks) layers. Panda's
 * preflight is the reset. The legacy `--background`, `--primary`, …
 * variables in src/styles/global/tokens.css alias the Panda semantic token
 * variables for the remaining `.chat-*` rules.
 *
 * Recipes: every preset recipe and variant is emitted (`staticCss`), so
 * `btn({ variant: props.tone })` works with runtime values. `css()` is
 * still extracted statically: pass it literal objects only.
 *
 * Breakpoints: the preset defines none (preset-base has no defaults), so
 * chat declares the usual four: `sm` 640, `md` 768 (sidebar inline),
 * `lg` 1024, `xl` 1280 (right-hand panel inline). Mobile-first:
 * `css({ display: "none", md: { display: "flex" } })`.
 *
 * Theme: `data-theme="night|daylight"` on <html>, night when absent
 * (preferences/theme.ts and the head script in layouts/AppLayout.astro).
 * Conditions: `_night`/`_dark`, `_daylight`/`_light`.
 */
import { defineConfig } from "@pandacss/dev";
import { waddlePreset } from "@waddle/design";

export default defineConfig({
  presets: ["@pandacss/preset-base", waddlePreset],
  preflight: true,
  include: ["./src/**/*.{ts,vue,astro}"],
  exclude: [],
  outdir: "styled-system",
  theme: {
    extend: {
      breakpoints: { sm: "640px", md: "768px", lg: "1024px", xl: "1280px" },
    },
  },
  staticCss: { recipes: "*" },
  layers: { reset: "pd_reset", base: "pd_base", tokens: "pd_tokens", recipes: "pd_recipes", utilities: "pd_utilities" },
});
