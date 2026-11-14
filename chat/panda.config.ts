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
 * Tailwind coexistence (Tailwind v4 stays installed until every component
 * is migrated):
 * - Layers are prefixed `pd_*` so they never merge with Tailwind's `base`
 *   and `utilities`. global.css fixes the order
 *   `theme, base, pd_*, components, utilities`: Panda beats Tailwind's
 *   reset, and Tailwind utilities on not-yet-migrated markup still beat
 *   Panda's element-level global CSS.
 * - Panda preflight is off; Tailwind's reset (preflight) is the one reset.
 *   Turn Panda's on when Tailwind is removed.
 * - The legacy `--background`, `--primary`, … variables in
 *   src/styles/global/tokens.css alias the Panda semantic token variables,
 *   so Tailwind utilities and the `.chat-*` sheets already use Polar Huddle
 *   colours.
 *
 * Recipes: every preset recipe and variant is emitted (`staticCss`), so
 * `btn({ variant: props.tone })` works with runtime values. `css()` is
 * still extracted statically: pass it literal objects only.
 *
 * Theme: `data-theme="night|daylight"` on <html>, night when absent
 * (preferences/theme.ts and the head script in layouts/AppLayout.astro).
 * Conditions: `_night`/`_dark`, `_daylight`/`_light`.
 */
import { defineConfig } from "@pandacss/dev";
import { waddlePreset } from "@waddle/design";

export default defineConfig({
  presets: ["@pandacss/preset-base", waddlePreset],
  preflight: false,
  include: ["./src/**/*.{ts,vue,astro}"],
  exclude: [],
  outdir: "styled-system",
  staticCss: { recipes: "*" },
  layers: { reset: "pd_reset", base: "pd_base", tokens: "pd_tokens", recipes: "pd_recipes", utilities: "pd_utilities" },
});
