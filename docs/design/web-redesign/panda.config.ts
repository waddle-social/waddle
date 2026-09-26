/**
 * Proposed Panda CSS configuration for Waddle Web ("Rookery" design language).
 *
 * This file is a design prototype. It lives under docs/ so it can be reviewed
 * without touching the chat workspace. When adopted, it moves to
 * chat/panda.config.ts and `@pandacss/vite` is added to chat/astro.config.mjs.
 *
 * Verified with `panda codegen` + `panda cssgen`:
 *   - @pandacss/dev@1.12.1: clean, every token reference resolves.
 *   - @pandacss/dev@2.0.0-beta.18: compiles with 0 diagnostics, but semantic
 *     tokens with conditional values ({ _light, _dark }) are left unresolved
 *     in the output (`background: action.subtle`). Stay on 1.x until fixed.
 * The authoring API is identical on both, so this file needs no change to move.
 */
import { defineConfig, defineRecipe, defineSlotRecipe } from "@pandacss/dev";

// ---------------------------------------------------------------------------
// Recipes (cva) — plain elements
// ---------------------------------------------------------------------------

const button = defineRecipe({
  className: "button",
  description: "Every clickable action. Ark UI has no Button; this is the recipe.",
  base: {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    gap: "2",
    fontWeight: "semibold",
    fontFamily: "ui",
    borderRadius: "control",
    cursor: "pointer",
    whiteSpace: "nowrap",
    transitionProperty: "background-color, color, border-color, box-shadow, transform",
    transitionDuration: "fast",
    _focusVisible: { outline: "2px solid", outlineColor: "ring", outlineOffset: "2px" },
    _disabled: { cursor: "default", opacity: 0.55 },
  },
  variants: {
    variant: {
      solid: { bg: "action", color: "action.fg", _hover: { bg: "action.hover" } },
      outline: { bg: "transparent", color: "action.text", borderWidth: "1px", borderColor: "action.text", _hover: { bg: "action.subtle" } },
      ghost: { bg: "transparent", color: "fg", _hover: { bg: "surface.hover" } },
      warm: { bg: "warm", color: "warm.fg", borderRadius: "pill", fontWeight: "bold", _hover: { bg: "warm.hover" } },
      danger: { bg: "danger", color: "danger.fg", _hover: { bg: "danger.hover" } },
    },
    size: {
      sm: { h: "control.sm", px: "3", fontSize: "control" },
      md: { h: "control.md", px: "4", fontSize: "body" },
      lg: { h: "control.lg", px: "5", fontSize: "message" },
    },
  },
  defaultVariants: { variant: "solid", size: "md" },
});

const badge = defineRecipe({
  className: "badge",
  base: {
    display: "inline-flex",
    alignItems: "center",
    gap: "1",
    borderRadius: "pill",
    px: "2.5",
    py: "1",
    fontSize: "meta",
    fontWeight: "bold",
    lineHeight: "compact",
  },
  variants: {
    tone: {
      teal: { bg: "action.subtle", color: "action.text" },
      warm: { bg: "warm.subtle", color: "warm.text" },
      neutral: { bg: "surface.2", color: "fg.muted" },
      mention: { bg: "warm", color: "warm.fg" },
      live: { bg: "transparent", color: "warm.text", borderWidth: "1px", borderColor: "warm" },
    },
  },
  defaultVariants: { tone: "neutral" },
});

// ---------------------------------------------------------------------------
// Slot recipes (sva) — Ark UI parts. Slot names match Ark's anatomy so the
// generated classes bind 1:1 to <Menu.Content>, <Tabs.Trigger>, etc.
// ---------------------------------------------------------------------------

const menu = defineSlotRecipe({
  className: "menu",
  slots: ["content", "item", "itemGroupLabel", "separator", "trigger"],
  base: {
    content: {
      minW: "60",
      p: "1.5",
      bg: "surface.raised",
      borderWidth: "1px",
      borderColor: "border",
      borderRadius: "panel",
      boxShadow: "floating",
      zIndex: "popover",
      outline: "none",
      _open: { animation: "fadeIn token(durations.fast) ease-out" },
    },
    item: {
      display: "flex",
      alignItems: "center",
      gap: "2.5",
      h: "control.md",
      px: "2.5",
      borderRadius: "control",
      fontSize: "body",
      color: "fg",
      cursor: "pointer",
      userSelect: "none",
      _highlighted: { bg: "surface.hover" },
      _disabled: { color: "fg.subtle", cursor: "default" },
      "&[data-tone=danger]": { color: "danger.text" },
    },
    itemGroupLabel: {
      px: "2.5",
      py: "2",
      fontSize: "caption",
      fontWeight: "semibold",
      letterSpacing: "wide",
      textTransform: "uppercase",
      color: "fg.muted",
    },
    separator: { h: "1px", bg: "border", mx: "1.5", my: "1" },
    trigger: { _focusVisible: { outline: "2px solid", outlineColor: "ring", outlineOffset: "2px" } },
  },
});

const tabs = defineSlotRecipe({
  className: "tabs",
  slots: ["root", "list", "trigger", "indicator", "content"],
  base: {
    list: { display: "flex", gap: "0.5", p: "0.5", borderRadius: "pill", bg: "surface.raised", borderWidth: "1px", borderColor: "border", w: "fit-content", position: "relative" },
    trigger: {
      h: "control.sm",
      px: "3.5",
      borderRadius: "pill",
      fontSize: "control",
      fontWeight: "medium",
      color: "fg.muted",
      cursor: "pointer",
      _selected: { color: "action.text", fontWeight: "semibold" },
      _focusVisible: { outline: "2px solid", outlineColor: "ring", outlineOffset: "2px" },
    },
    indicator: { bg: "action.subtle", borderRadius: "pill", zIndex: "-1" },
    content: { outline: "none" },
  },
});

const avatar = defineSlotRecipe({
  className: "avatar",
  slots: ["root", "image", "fallback", "presence"],
  base: {
    root: { position: "relative", display: "inline-flex", flexShrink: 0 },
    image: { w: "full", h: "full", borderRadius: "pill", objectFit: "cover" },
    fallback: { w: "full", h: "full", borderRadius: "pill", display: "flex", alignItems: "center", justifyContent: "center", fontWeight: "bold", color: "ink.900", bg: "action.subtle" },
    presence: {
      position: "absolute",
      right: "-1px",
      bottom: "-1px",
      w: "3",
      h: "3",
      borderRadius: "pill",
      borderWidth: "2px",
      borderColor: "bg",
      "&[data-show=available], &[data-show=chat]": { bg: "presence.available" },
      "&[data-show=away], &[data-show=xa]": { bg: "presence.away" },
      "&[data-show=dnd]": { bg: "presence.dnd" },
      "&[data-show=offline]": { bg: "presence.offline" },
    },
  },
  variants: {
    size: {
      sm: { root: { w: "7", h: "7" }, fallback: { fontSize: "avatar.sm" } },
      md: { root: { w: "9", h: "9" }, fallback: { fontSize: "avatar.md" } },
      lg: { root: { w: "12", h: "12" }, fallback: { fontSize: "avatar.lg" } },
    },
    inHuddle: {
      true: { root: { boxShadow: "0 0 0 2px token(colors.bg), 0 0 0 4px token(colors.action)" } },
    },
  },
  defaultVariants: { size: "md" },
});

const toast = defineSlotRecipe({
  className: "toast",
  slots: ["root", "title", "description", "actionTrigger", "closeTrigger"],
  base: {
    root: {
      display: "flex",
      alignItems: "flex-start",
      gap: "3",
      p: "3.5",
      bg: "surface.raised",
      borderWidth: "1px",
      borderColor: "border",
      borderRadius: "panel",
      boxShadow: "floating",
      color: "fg",
      minW: "80",
    },
    title: { fontSize: "body", fontWeight: "semibold" },
    description: { fontSize: "control", color: "fg.muted" },
    actionTrigger: { h: "control.xs", px: "2.5", borderRadius: "control", bg: "action.subtle", color: "action.text", fontWeight: "bold", fontSize: "control" },
    closeTrigger: { color: "fg.muted", _hover: { color: "fg" } },
  },
});

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export default defineConfig({
  presets: ["@pandacss/preset-base", "@pandacss/preset-panda"],
  preflight: true,
  jsxFramework: "vue",
  include: ["./src/**/*.{ts,vue,astro}"],
  exclude: [],
  outdir: "styled-system",
  // Tailwind v4 also declares `base` and `utilities` layers. Prefix Panda's
  // layers so the two systems never merge into one cascade layer while both
  // are installed during migration.
  layers: {
    reset: "pd_reset",
    base: "pd_base",
    tokens: "pd_tokens",
    recipes: "pd_recipes",
    utilities: "pd_utilities",
  },
  conditions: {
    extend: {
      // Mirrors the existing data-theme + system fallback in AppLayout.astro.
      light: "[data-theme=light] &, :root:not([data-theme=dark]) &",
      dark: "[data-theme=dark] &, :root:not([data-theme=light]) &",
      highlighted: "&[data-highlighted]",
      selected: "&[data-selected]",
      open: "&[data-state=open]",
      density: "[data-density=compact] &",
    },
  },
  theme: {
    extend: {
      tokens: {
        colors: {
          ink: {
            900: { value: "#0b1219" },
            800: { value: "#121f2b" },
            700: { value: "#1a2a3a" },
            600: { value: "#26384a" },
            500: { value: "#4a5966" },
            400: { value: "#9fb0bd" },
            300: { value: "#c9d4dc" },
            200: { value: "#d9e2e6" },
            100: { value: "#e9eff1" },
            50: { value: "#f2f6f7" },
            0: { value: "#ffffff" },
          },
          sand: { 50: { value: "#f8f4ec" }, 100: { value: "#fffdf8" }, 200: { value: "#e6dfd2" } },
          teal: {
            700: { value: "#0b7a6f" },
            600: { value: "#0f8f82" },
            500: { value: "#12a596" },
            300: { value: "#6fd3c7" },
            200: { value: "#a3e6de" },
            100: { value: "#d6f3ef" },
            900: { value: "#123f3a" },
          },
          beak: {
            700: { value: "#b04a06" },
            500: { value: "#ff7f21" },
            300: { value: "#ffa057" },
            200: { value: "#ffc59a" },
            100: { value: "#ffe3cd" },
            50: { value: "#fff7f0" },
            900: { value: "#4a2a12" },
          },
          green: { 500: { value: "#1f9d55" } },
          amber: { 500: { value: "#d97706" } },
          red: { 500: { value: "#d1434b" }, 700: { value: "#b8353d" }, 300: { value: "#ff8b90" } },
        },
        fonts: {
          display: { value: "'Fredoka', 'Outfit Variable', system-ui, sans-serif" },
          ui: { value: "'Outfit Variable', system-ui, sans-serif" },
          code: { value: "'JetBrains Mono Variable', ui-monospace, monospace" },
        },
        fontSizes: {
          caption: { value: "0.6875rem" },
          meta: { value: "0.75rem" },
          control: { value: "0.8125rem" },
          body: { value: "0.875rem" },
          message: { value: "0.9375rem" },
          title: { value: "1rem" },
          display: { value: "1.25rem" },
          hero: { value: "2rem" },
          "avatar.sm": { value: "0.6875rem" },
          "avatar.md": { value: "0.75rem" },
          "avatar.lg": { value: "1rem" },
        },
        lineHeights: {
          compact: { value: "1.25" },
          control: { value: "1.35" },
          body: { value: "1.5" },
        },
        radii: {
          control: { value: "0.625rem" },
          panel: { value: "0.875rem" },
          card: { value: "1rem" },
          hero: { value: "1.125rem" },
          pill: { value: "999px" },
        },
        sizes: {
          "control.xs": { value: "1.75rem" },
          "control.sm": { value: "2rem" },
          "control.md": { value: "2.25rem" },
          "control.lg": { value: "2.5rem" },
          "control.touch": { value: "2.75rem" },
          rail: { value: "4.5rem" },
          sidebar: { value: "clamp(16.5rem, 18vw, 19rem)" },
          people: { value: "16.75rem" },
        },
        durations: { fast: { value: "150ms" }, normal: { value: "220ms" } },
        shadows: {
          floating: { value: "0 12px 32px rgba(15, 26, 36, 0.12)" },
          elevated: { value: "0 18px 50px rgba(15, 26, 36, 0.16)" },
        },
        zIndex: { sticky: { value: 10 }, floating: { value: 20 }, popover: { value: 50 }, modal: { value: 60 }, lightbox: { value: 70 } },
      },
      semanticTokens: {
        colors: {
          bg: { value: { _light: "{colors.ink.50}", _dark: "{colors.ink.900}" } },
          fg: {
            DEFAULT: { value: { _light: "#0f1a24", _dark: "#e8eef2" } },
            muted: { value: { _light: "{colors.ink.500}", _dark: "{colors.ink.400}" } },
            subtle: { value: { _light: "{colors.ink.400}", _dark: "{colors.ink.500}" } },
          },
          surface: {
            DEFAULT: { value: { _light: "{colors.ink.0}", _dark: "{colors.ink.800}" } },
            raised: { value: { _light: "{colors.ink.0}", _dark: "{colors.ink.800}" } },
            2: { value: { _light: "{colors.ink.100}", _dark: "{colors.ink.700}" } },
            hover: { value: { _light: "{colors.ink.50}", _dark: "{colors.ink.700}" } },
            rail: { value: { _light: "{colors.ink.800}", _dark: "#0f1a24" } },
          },
          border: { value: { _light: "{colors.ink.200}", _dark: "{colors.ink.600}" } },
          ring: { value: { _light: "{colors.teal.500}", _dark: "{colors.teal.300}" } },
          action: {
            DEFAULT: { value: { _light: "{colors.teal.700}", _dark: "{colors.teal.300}" } },
            hover: { value: { _light: "#075f57", _dark: "{colors.teal.200}" } },
            fg: { value: { _light: "{colors.ink.0}", _dark: "{colors.ink.900}" } },
            text: { value: { _light: "{colors.teal.700}", _dark: "{colors.teal.300}" } },
            subtle: { value: { _light: "{colors.teal.100}", _dark: "{colors.teal.900}" } },
          },
          warm: {
            DEFAULT: { value: "{colors.beak.500}" },
            hover: { value: "#f0701a" },
            fg: { value: "{colors.ink.900}" },
            text: { value: { _light: "{colors.beak.700}", _dark: "{colors.beak.300}" } },
            subtle: { value: { _light: "{colors.beak.100}", _dark: "{colors.beak.900}" } },
          },
          danger: {
            DEFAULT: { value: "{colors.red.500}" },
            hover: { value: "{colors.red.700}" },
            fg: { value: "{colors.ink.0}" },
            text: { value: { _light: "{colors.red.700}", _dark: "{colors.red.300}" } },
          },
          presence: {
            available: { value: "{colors.green.500}" },
            away: { value: "{colors.amber.500}" },
            dnd: { value: "{colors.red.500}" },
            offline: { value: { _light: "{colors.ink.300}", _dark: "{colors.ink.500}" } },
          },
        },
      },
      keyframes: {
        fadeIn: { from: { opacity: 0, transform: "translateY(2px)" }, to: { opacity: 1, transform: "none" } },
      },
      recipes: { button, badge },
      slotRecipes: { menu, tabs, avatar, toast },
    },
  },
  globalCss: {
    html: { colorScheme: "light dark" },
    body: {
      bg: "bg",
      color: "fg",
      fontFamily: "ui",
      fontSize: "body",
      lineHeight: "body",
      WebkitFontSmoothing: "antialiased",
    },
  },
  staticCss: {
    recipes: { button: [{ variant: ["*"], size: ["*"] }], badge: [{ tone: ["*"] }] },
  },
});
