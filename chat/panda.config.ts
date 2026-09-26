/**
 * Huddle — proposed Panda CSS configuration for Waddle Web.
 *
 * Wired through @pandacss/postcss in astro.config.mjs (vite.css.postcss). Generated output
 * lands in ./styled-system (gitignored; `bun run panda:codegen`).
 * Tailwind v4 is still installed during the migration: Panda's cascade
 * layers are prefixed pd_* so the two never merge, and Panda's preflight
 * is off because Tailwind's reset already applies.
 *
 * Night (dark) is the canonical theme; daylight is the `_light` condition.
 *
 * Verified with `panda codegen` + `panda cssgen` on @pandacss/dev@1.12.1.
 * The v2 beta (2.0.0-beta.18) compiles it but leaves conditional semantic
 * tokens unresolved in the output, so stay on 1.x until that is fixed.
 */
import { defineConfig, defineRecipe, defineSlotRecipe } from "@pandacss/dev";

// ---------------------------------------------------------------------------
// Recipes — plain elements
// ---------------------------------------------------------------------------

const button = defineRecipe({
  className: "button",
  description: "Every clickable action. Ark UI ships no Button; this is it.",
  base: {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    gap: "2",
    fontFamily: "body",
    fontWeight: "semibold",
    borderRadius: "control",
    cursor: "pointer",
    whiteSpace: "nowrap",
    transitionProperty: "background-color, color, border-color, box-shadow",
    transitionDuration: "fast",
    transitionTimingFunction: "out",
    _focusVisible: { outline: "2px solid", outlineColor: "action", outlineOffset: "2px" },
    _disabled: { cursor: "default", opacity: 0.5 },
  },
  variants: {
    variant: {
      primary: { bg: "action", color: "action.fg", fontWeight: "bold", _hover: { bg: "action.hover" } },
      secondary: { bg: "transparent", color: "action.text", borderWidth: "1px", borderColor: "action.text", _hover: { bg: "action.subtle" } },
      quiet: { bg: "surface", color: "fg", borderWidth: "1px", borderColor: "border", _hover: { bg: "surface.hover" } },
      live: { bg: "live", color: "live.fg", borderRadius: "pill", fontWeight: "bold", _hover: { bg: "live.hover" } },
      liveOutline: { bg: "transparent", color: "live.text", borderWidth: "1px", borderColor: "live.text", borderRadius: "pill", fontWeight: "bold" },
      danger: { bg: "danger", color: "danger.fg", fontWeight: "bold", _hover: { bg: "danger.hover" } },
      kudos: { bg: "gold.subtle", color: "gold.text", borderWidth: "1px", borderColor: "gold", borderRadius: "pill" },
    },
    size: {
      sm: { h: "control.sm", px: "3", fontSize: "control" },
      md: { h: "control.md", px: "4", fontSize: "body" },
      lg: { h: "control.lg", px: "5", fontSize: "message" },
    },
  },
  defaultVariants: { variant: "primary", size: "md" },
});

const tag = defineRecipe({
  className: "tag",
  description: "Role, state and live labels. Mono, uppercase, hairline.",
  base: {
    display: "inline-flex",
    alignItems: "center",
    gap: "1",
    fontFamily: "mono",
    fontSize: "kicker",
    letterSpacing: "kicker",
    textTransform: "uppercase",
    lineHeight: "1",
    px: "1.5",
    py: "0.5",
    borderRadius: "tag",
    borderWidth: "1px",
    borderColor: "border",
    color: "fg.muted",
  },
  variants: {
    tone: {
      host: { color: "action.text", borderColor: "action.text" },
      helper: { color: "gold.text", borderColor: "gold" },
      new: { color: "moss.text" },
      answered: { color: "moss.text", borderColor: "moss.text" },
      accepted: { color: "moss.fg", bg: "moss", borderColor: "moss" },
      live: { color: "live.text", borderColor: "live.text", borderRadius: "pill" },
      neutral: {},
    },
  },
  defaultVariants: { tone: "neutral" },
});

const kicker = defineRecipe({
  className: "kicker",
  description: "Section labels: “Happening now”, “Around · 37”.",
  base: { fontFamily: "mono", fontSize: "kicker", fontWeight: "medium", letterSpacing: "kicker", textTransform: "uppercase", color: "fg.muted" },
  variants: { tone: { live: { color: "live.text" }, action: { color: "action.text" } } },
});

const count = defineRecipe({
  className: "count",
  description: "Unread and mention badges.",
  base: { minW: "4.5", h: "4.5", px: "1.5", borderRadius: "pill", bg: "live", color: "live.fg", fontSize: "kicker", fontWeight: "bold", display: "inline-flex", alignItems: "center", justifyContent: "center" },
});

// ---------------------------------------------------------------------------
// Slot recipes — Ark UI anatomies. Slot names match Ark parts 1:1.
// ---------------------------------------------------------------------------

const nav = defineSlotRecipe({
  className: "nav",
  description: "Ark Tabs used as the community's primary navigation (pill).",
  slots: ["root", "list", "trigger", "indicator", "content"],
  base: {
    list: { display: "flex", gap: "0.5", position: "relative" },
    trigger: {
      display: "flex",
      alignItems: "center",
      gap: "1.5",
      px: "3",
      py: "2",
      borderRadius: "pill",
      fontSize: "body",
      fontWeight: "medium",
      color: "fg.muted",
      cursor: "pointer",
      _selected: { color: "fg", fontWeight: "semibold" },
      _focusVisible: { outline: "2px solid", outlineColor: "action", outlineOffset: "2px" },
    },
    indicator: { bg: "surface.2", borderRadius: "pill", zIndex: -1 },
    content: { outline: "none" },
  },
});

const segment = defineSlotRecipe({
  className: "segment",
  description: "Ark SegmentGroup as a pill filter (Here now / Helpers / Everyone).",
  slots: ["root", "item", "itemText", "indicator"],
  base: {
    root: { display: "inline-flex", gap: "1", p: "0.5", borderRadius: "pill", borderWidth: "1px", borderColor: "border", bg: "surface", position: "relative" },
    item: { h: "control.xs", px: "3", borderRadius: "pill", fontSize: "control", fontWeight: "semibold", color: "fg.muted", cursor: "pointer", display: "flex", alignItems: "center", _checked: { color: "fg" } },
    indicator: { bg: "surface.2", borderRadius: "pill", zIndex: 0 },
    itemText: { position: "relative", zIndex: 1 },
  },
});

const menu = defineSlotRecipe({
  className: "menu",
  slots: ["content", "item", "itemGroupLabel", "separator", "trigger"],
  base: {
    content: { minW: "58", p: "1.5", bg: "surface", borderWidth: "1px", borderColor: "border", borderRadius: "panel", boxShadow: "overlay", zIndex: "popover", outline: "none" },
    item: { display: "flex", alignItems: "center", gap: "2.5", h: "control.sm", px: "2.5", borderRadius: "control", fontSize: "body", color: "fg", cursor: "pointer", _highlighted: { bg: "surface.2" }, "&[data-tone=danger]": { color: "danger.text" } },
    itemGroupLabel: { px: "2.5", pt: "2", pb: "1.5", fontFamily: "mono", fontSize: "kicker", letterSpacing: "kicker", textTransform: "uppercase", color: "fg.muted" },
    separator: { h: "1px", bg: "border", mx: "1", my: "1" },
    trigger: { _focusVisible: { outline: "2px solid", outlineColor: "action", outlineOffset: "2px" } },
  },
});

const avatar = defineSlotRecipe({
  className: "avatar",
  description: "Ark Avatar plus a presence dot. A ring means in a huddle; `speaking` adds the glow.",
  slots: ["root", "image", "fallback", "presence"],
  base: {
    root: { position: "relative", display: "inline-flex", flexShrink: 0, borderRadius: "pill" },
    image: { w: "full", h: "full", borderRadius: "pill", objectFit: "cover" },
    fallback: { w: "full", h: "full", borderRadius: "pill", display: "flex", alignItems: "center", justifyContent: "center", fontWeight: "bold", color: "ink.900", bg: "avatar.fallback" },
    presence: {
      position: "absolute",
      right: "-1px",
      bottom: "-1px",
      w: "3",
      h: "3",
      borderRadius: "pill",
      borderWidth: "2px",
      borderColor: "bg",
      bg: "presence.available",
      "&[data-show=away], &[data-show=xa]": { bg: "presence.away" },
      "&[data-show=dnd]": { bg: "presence.dnd" },
      "&[data-show=offline]": { bg: "bg", borderWidth: "1.5px", borderColor: "presence.offline" },
    },
  },
  variants: {
    size: {
      sm: { root: { w: "7", h: "7" }, fallback: { fontSize: "kicker" } },
      md: { root: { w: "8", h: "8" }, fallback: { fontSize: "caption" } },
      lg: { root: { w: "11", h: "11" }, fallback: { fontSize: "message" } },
      xl: { root: { w: "16", h: "16" }, fallback: { fontFamily: "display", fontSize: "title" } },
    },
    huddle: {
      true: { root: { boxShadow: "0 0 0 2px token(colors.bg), 0 0 0 4px token(colors.action)" } },
    },
    speaking: {
      true: { root: { boxShadow: "0 0 0 2px token(colors.bg), 0 0 0 4px token(colors.action), 0 0 18px 2px token(colors.glow.action)" } },
    },
  },
  defaultVariants: { size: "md" },
});

const card = defineSlotRecipe({
  className: "card",
  description: "Room, event and question cards. `live` gets the ember border and glow.",
  slots: ["root", "kicker", "title", "body", "footer"],
  base: {
    root: { display: "flex", flexDirection: "column", gap: "2.5", p: "4", borderRadius: "card", bg: "surface", borderWidth: "1px", borderColor: "border" },
    kicker: { fontFamily: "mono", fontSize: "kicker", letterSpacing: "kicker", textTransform: "uppercase", color: "fg.muted" },
    title: { fontFamily: "display", fontWeight: "semibold", fontSize: "title", lineHeight: "snug" },
    body: { fontSize: "control", color: "fg.muted", lineHeight: "body" },
    footer: { display: "flex", alignItems: "center", gap: "2.5", mt: "auto" },
  },
  variants: {
    tone: {
      live: { root: { borderColor: "live", boxShadow: "0 0 28px -10px token(colors.glow.live)" }, kicker: { color: "live.text" } },
      active: { kicker: { color: "action.text" } },
      quiet: {},
    },
  },
});

const toast = defineSlotRecipe({
  className: "toast",
  slots: ["root", "title", "description", "actionTrigger", "closeTrigger"],
  base: {
    root: { display: "flex", alignItems: "flex-start", gap: "3", p: "3.5", bg: "toast.bg", color: "toast.fg", borderWidth: "1px", borderColor: "live", borderRadius: "panel", boxShadow: "overlay", minW: "80" },
    title: { fontSize: "body", fontWeight: "semibold" },
    description: { fontSize: "control", color: "toast.muted" },
    actionTrigger: { h: "control.xs", px: "2.5", borderRadius: "pill", bg: "live", color: "live.fg", fontWeight: "bold", fontSize: "caption" },
    closeTrigger: { color: "toast.muted", _hover: { color: "toast.fg" } },
  },
});

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export default defineConfig({
  presets: ["@pandacss/preset-base", "@pandacss/preset-panda"],
  preflight: false,
  include: ["./src/**/*.{ts,vue,astro}"],
  exclude: [],
  outdir: "styled-system",
  // Tailwind v4 also declares `base` and `utilities`. Prefix Panda's layers
  // so the two never merge into one cascade layer while both are installed.
  layers: { reset: "pd_reset", base: "pd_base", tokens: "pd_tokens", recipes: "pd_recipes", utilities: "pd_utilities" },
  conditions: {
    extend: {
      // Night is the default; daylight is opt-in or system-light.
      dark: "[data-theme=dark] &, :root:not([data-theme=light]) &",
      light: "[data-theme=light] &",
      highlighted: "&[data-highlighted]",
      selected: "&[data-selected]",
      checked: "&[data-state=checked]",
      density: "[data-density=compact] &",
      motionOk: "@media (prefers-reduced-motion: no-preference)",
    },
  },
  theme: {
    extend: {
      tokens: {
        colors: {
          ink: {
            950: { value: "#070c12" },
            900: { value: "#0b1219" },
            850: { value: "#0f1a24" },
            800: { value: "#121f2b" },
            700: { value: "#1a2a3a" },
            650: { value: "#1f2f3f" },
            600: { value: "#26384a" },
            500: { value: "#3a4f63" },
            400: { value: "#6b7f92" },
            300: { value: "#9fb0bd" },
            200: { value: "#c9d4dc" },
            100: { value: "#e8eef2" },
            50: { value: "#f2f6f7" },
            0: { value: "#ffffff" },
          },
          daylight: { border: { value: "#d9e2e6" }, muted: { value: "#4a5966" }, fg: { value: "#0f1a24" } },
          teal: { 900: { value: "#123f3a" }, 700: { value: "#0b7a6f" }, 600: { value: "#075f57" }, 500: { value: "#12a596" }, 300: { value: "#6fd3c7" }, 200: { value: "#a3e6de" }, 100: { value: "#d6f3ef" } },
          ember: { 900: { value: "#4a2a12" }, 700: { value: "#b04a06" }, 500: { value: "#ff7f21" }, 400: { value: "#ff9440" }, 300: { value: "#ffa057" }, 100: { value: "#ffe3cd" } },
          gold: { 900: { value: "#3d3320" }, 700: { value: "#8a5a0a" }, 500: { value: "#e9b44c" }, 100: { value: "#fdf3dc" } },
          moss: { 700: { value: "#2f6b3f" }, 300: { value: "#7fc98f" } },
          green: { 500: { value: "#3ddc84" }, 700: { value: "#1f9d55" } },
          amber: { 500: { value: "#f2b036" }, 700: { value: "#d97706" } },
          red: { 700: { value: "#b8353d" }, 500: { value: "#d1434b" }, 300: { value: "#ff8b90" } },
          avatarTints: { 1: { value: "#d6f3ef" }, 2: { value: "#ffe3cd" }, 3: { value: "#e3e8f7" }, 4: { value: "#c9d4dc" } },
        },
        fonts: {
          display: { value: "'Space Grotesk Variable', system-ui, sans-serif" },
          body: { value: "'Outfit Variable', system-ui, sans-serif" },
          mono: { value: "'JetBrains Mono Variable', ui-monospace, monospace" },
        },
        fontSizes: {
          kicker: { value: "0.6875rem" },
          caption: { value: "0.75rem" },
          control: { value: "0.8125rem" },
          body: { value: "0.875rem" },
          message: { value: "0.9375rem" },
          lead: { value: "1.0625rem" },
          title: { value: "1.125rem" },
          headline: { value: "1.875rem" },
          display: { value: "2.375rem" },
          hero: { value: "3.25rem" },
        },
        lineHeights: { tight: { value: "1" }, snug: { value: "1.2" }, body: { value: "1.5" } },
        letterSpacings: { display: { value: "-0.03em" }, title: { value: "-0.01em" }, kicker: { value: "0.08em" } },
        radii: { tag: { value: "0.25rem" }, control: { value: "0.625rem" }, panel: { value: "0.75rem" }, card: { value: "1rem" }, pill: { value: "999px" } },
        sizes: {
          "control.xs": { value: "1.75rem" },
          "control.sm": { value: "2rem" },
          "control.md": { value: "2.25rem" },
          "control.lg": { value: "2.5rem" },
          "control.touch": { value: "2.75rem" },
          people: { value: "17.5rem" },
          context: { value: "20rem" },
          "context.wide": { value: "23.75rem" },
          measure: { value: "70ch" },
        },
        durations: { fast: { value: "160ms" }, normal: { value: "240ms" }, enter: { value: "400ms" } },
        easings: { out: { value: "cubic-bezier(0.2, 0.7, 0.2, 1)" } },
        shadows: { overlay: { value: "0 16px 40px rgba(0, 0, 0, 0.45)" } },
        zIndex: { sticky: { value: 10 }, floating: { value: 20 }, popover: { value: 50 }, modal: { value: 60 }, lightbox: { value: 70 } },
      },
      semanticTokens: {
        colors: {
          bg: { value: { _dark: "{colors.ink.900}", _light: "{colors.ink.50}" } },
          "bg.rail": { value: { _dark: "{colors.ink.850}", _light: "{colors.ink.0}" } },
          fg: {
            DEFAULT: { value: { _dark: "{colors.ink.100}", _light: "{colors.daylight.fg}" } },
            muted: { value: { _dark: "{colors.ink.300}", _light: "{colors.daylight.muted}" } },
            soft: { value: { _dark: "{colors.ink.400}", _light: "{colors.ink.300}" } },
          },
          surface: {
            DEFAULT: { value: { _dark: "{colors.ink.800}", _light: "{colors.ink.0}" } },
            2: { value: { _dark: "{colors.ink.700}", _light: "{colors.ink.50}" } },
            hover: { value: { _dark: "{colors.ink.700}", _light: "{colors.ink.50}" } },
            code: { value: { _dark: "{colors.ink.950}", _light: "{colors.ink.50}" } },
          },
          border: {
            DEFAULT: { value: { _dark: "{colors.ink.600}", _light: "{colors.daylight.border}" } },
            soft: { value: { _dark: "{colors.ink.650}", _light: "{colors.daylight.border}" } },
          },
          action: {
            DEFAULT: { value: { _dark: "{colors.teal.300}", _light: "{colors.teal.700}" } },
            hover: { value: { _dark: "{colors.teal.200}", _light: "{colors.teal.600}" } },
            fg: { value: { _dark: "{colors.ink.900}", _light: "{colors.ink.0}" } },
            text: { value: { _dark: "{colors.teal.300}", _light: "{colors.teal.700}" } },
            subtle: { value: { _dark: "{colors.teal.900}", _light: "{colors.teal.100}" } },
          },
          live: {
            DEFAULT: { value: { _dark: "{colors.ember.300}", _light: "{colors.ember.500}" } },
            hover: { value: { _dark: "{colors.ember.400}", _light: "{colors.ember.400}" } },
            fg: { value: "{colors.ink.900}" },
            text: { value: { _dark: "{colors.ember.300}", _light: "{colors.ember.700}" } },
            subtle: { value: { _dark: "{colors.ember.900}", _light: "{colors.ember.100}" } },
          },
          gold: {
            DEFAULT: { value: "{colors.gold.500}" },
            text: { value: { _dark: "{colors.gold.500}", _light: "{colors.gold.700}" } },
            subtle: { value: { _dark: "{colors.gold.900}", _light: "{colors.gold.100}" } },
          },
          moss: {
            DEFAULT: { value: { _dark: "{colors.moss.300}", _light: "{colors.moss.700}" } },
            fg: { value: { _dark: "{colors.ink.900}", _light: "{colors.ink.0}" } },
            text: { value: { _dark: "{colors.moss.300}", _light: "{colors.moss.700}" } },
          },
          danger: {
            DEFAULT: { value: "{colors.red.500}" },
            hover: { value: "{colors.red.700}" },
            fg: { value: "{colors.ink.0}" },
            text: { value: { _dark: "{colors.red.300}", _light: "{colors.red.700}" } },
          },
          presence: {
            available: { value: { _dark: "{colors.green.500}", _light: "{colors.green.700}" } },
            away: { value: { _dark: "{colors.amber.500}", _light: "{colors.amber.700}" } },
            dnd: { value: { _dark: "{colors.red.300}", _light: "{colors.red.500}" } },
            offline: { value: { _dark: "{colors.ink.400}", _light: "{colors.ink.300}" } },
          },
          glow: {
            live: { value: "rgba(255, 160, 87, 0.55)" },
            action: { value: "rgba(111, 211, 199, 0.45)" },
          },
          // Toasts and tooltips are always night-coloured: live things are photographed at night.
          toast: { bg: { value: "{colors.ink.800}" }, fg: { value: "{colors.ink.100}" }, muted: { value: "{colors.ink.300}" } },
          avatar: { fallback: { value: "{colors.avatarTints.1}" } },
        },
      },
      textStyles: {
        hero: { value: { fontFamily: "display", fontSize: "hero", fontWeight: "700", lineHeight: "tight", letterSpacing: "display" } },
        display: { value: { fontFamily: "display", fontSize: "display", fontWeight: "700", lineHeight: "tight", letterSpacing: "display" } },
        headline: { value: { fontFamily: "display", fontSize: "headline", fontWeight: "700", lineHeight: "tight", letterSpacing: "display" } },
        title: { value: { fontFamily: "display", fontSize: "title", fontWeight: "600", lineHeight: "snug", letterSpacing: "title" } },
        stat: { value: { fontFamily: "display", fontSize: "headline", fontWeight: "700", lineHeight: "tight" } },
        lead: { value: { fontFamily: "body", fontSize: "lead", lineHeight: "body" } },
        body: { value: { fontFamily: "body", fontSize: "message", lineHeight: "body" } },
        control: { value: { fontFamily: "body", fontSize: "body", fontWeight: "500", lineHeight: "snug" } },
        kicker: { value: { fontFamily: "mono", fontSize: "kicker", fontWeight: "500", letterSpacing: "kicker", textTransform: "uppercase" } },
        time: { value: { fontFamily: "mono", fontSize: "caption", lineHeight: "snug" } },
      },
      keyframes: {
        speaking: { "0%, 100%": { transform: "scaleY(0.4)" }, "50%": { transform: "scaleY(1)" } },
        enter: { from: { opacity: 0, transform: "translateY(4px)" }, to: { opacity: 1, transform: "none" } },
      },
      recipes: { button, tag, kicker, count },
      slotRecipes: { nav, segment, menu, avatar, card, toast },
    },
  },
  staticCss: { recipes: { button: [{ variant: ["*"], size: ["*"] }], tag: [{ tone: ["*"] }], card: [{ tone: ["*"] }] } },
});
