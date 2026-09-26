/**
 * Hearth — proposed Panda CSS configuration for Waddle Web.
 *
 * Design prototype. Lives under docs/ so it can be reviewed without touching
 * the chat workspace. When adopted it moves to chat/panda.config.ts and
 * `@pandacss/vite` is added to chat/astro.config.mjs.
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
    transitionProperty: "background-color, color, border-color, outline-color",
    transitionDuration: "fast",
    _focusVisible: { outline: "2px solid", outlineColor: "ember", outlineOffset: "2px" },
    _disabled: { cursor: "default", opacity: 0.5 },
  },
  variants: {
    variant: {
      primary: { bg: "ember", color: "ember.fg", _hover: { bg: "ember.hover" } },
      secondary: { bg: "transparent", color: "fg", borderWidth: "1px", borderColor: "fg", _hover: { bg: "surface.hover" } },
      quiet: { bg: "transparent", color: "fg.muted", _hover: { color: "fg", bg: "surface.hover" } },
      danger: { bg: "danger", color: "danger.fg", _hover: { bg: "danger.hover" } },
      kudos: { bg: "gold.subtle", color: "gold.text", borderWidth: "1px", borderColor: "gold", borderRadius: "pill" },
    },
    size: {
      sm: { h: "control.sm", px: "3", fontSize: "control" },
      md: { h: "control.md", px: "4", fontSize: "body" },
      lg: { h: "control.lg", px: "5", fontSize: "message" },
    },
    shape: { pill: { borderRadius: "pill" } },
  },
  defaultVariants: { variant: "primary", size: "md" },
});

const tag = defineRecipe({
  className: "tag",
  description: "Role, state and count labels. Mono, uppercase, hairline.",
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
      host: { color: "ember.text", borderColor: "ember" },
      helper: { color: "gold.text", borderColor: "gold" },
      answered: { color: "moss.text", borderColor: "moss.text" },
      accepted: { color: "moss.fg", bg: "moss", borderColor: "moss" },
      new: { color: "moss.text", borderColor: "border" },
      neutral: {},
    },
  },
  defaultVariants: { tone: "neutral" },
});

const kicker = defineRecipe({
  className: "kicker",
  description: "Numbered section labels: “01 — Needs someone like you”.",
  base: {
    fontFamily: "mono",
    fontSize: "kicker",
    fontWeight: "medium",
    letterSpacing: "kicker",
    textTransform: "uppercase",
    color: "fg.muted",
  },
});

// ---------------------------------------------------------------------------
// Slot recipes — Ark UI anatomies. Slot names match Ark parts 1:1.
// ---------------------------------------------------------------------------

const nav = defineSlotRecipe({
  className: "nav",
  description: "Ark Tabs used as the community's primary navigation (underline).",
  slots: ["root", "list", "trigger", "indicator", "content"],
  base: {
    list: { display: "flex", gap: "6", position: "relative", alignItems: "stretch" },
    trigger: {
      display: "flex",
      alignItems: "center",
      fontSize: "body",
      fontWeight: "medium",
      color: "fg.muted",
      cursor: "pointer",
      py: "2",
      _selected: { color: "fg", fontWeight: "semibold" },
      _focusVisible: { outline: "2px solid", outlineColor: "ember", outlineOffset: "2px" },
    },
    indicator: { height: "2px", bg: "ember", bottom: "-1px" },
    content: { outline: "none" },
  },
});

const segment = defineSlotRecipe({
  className: "segment",
  description: "Ark SegmentGroup as a pill filter (Here now / Helpers / Everyone).",
  slots: ["root", "item", "itemText", "indicator"],
  base: {
    root: { display: "inline-flex", gap: "1", p: "0.5", borderRadius: "pill", borderWidth: "1px", borderColor: "border", bg: "surface", position: "relative" },
    item: { h: "control.xs", px: "3", borderRadius: "pill", fontSize: "control", fontWeight: "semibold", color: "fg.muted", cursor: "pointer", display: "flex", alignItems: "center", _checked: { color: "bg" } },
    indicator: { bg: "fg", borderRadius: "pill", zIndex: 0 },
    itemText: { position: "relative", zIndex: 1 },
  },
});

const menu = defineSlotRecipe({
  className: "menu",
  slots: ["content", "item", "itemGroupLabel", "separator", "trigger"],
  base: {
    content: { minW: "58", p: "1.5", bg: "surface", borderWidth: "1px", borderColor: "fg", borderRadius: "panel", boxShadow: "overlay", zIndex: "popover", outline: "none" },
    item: { display: "flex", alignItems: "center", gap: "2.5", h: "control.sm", px: "2.5", borderRadius: "tag", fontSize: "body", color: "fg", cursor: "pointer", _highlighted: { bg: "surface.2" }, "&[data-tone=danger]": { color: "danger.text" } },
    itemGroupLabel: { px: "2.5", pt: "2", pb: "1.5", fontFamily: "mono", fontSize: "kicker", letterSpacing: "kicker", textTransform: "uppercase", color: "fg.muted" },
    separator: { h: "1px", bg: "border", mx: "1", my: "1" },
    trigger: { _focusVisible: { outline: "2px solid", outlineColor: "ember", outlineOffset: "2px" } },
  },
});

const avatar = defineSlotRecipe({
  className: "avatar",
  description: "Ark Avatar plus a presence slot. Presence is a shape, not a colour.",
  slots: ["root", "image", "fallback", "presence"],
  base: {
    root: { position: "relative", display: "inline-flex", flexShrink: 0 },
    image: { w: "full", h: "full", borderRadius: "pill", objectFit: "cover" },
    fallback: { w: "full", h: "full", borderRadius: "pill", display: "flex", alignItems: "center", justifyContent: "center", fontWeight: "bold", color: "ink", bg: "surface.2" },
    presence: {
      position: "absolute",
      right: "-2px",
      bottom: "-2px",
      w: "3.5",
      h: "3.5",
      borderRadius: "pill",
      boxSizing: "border-box",
      outline: "2px solid",
      outlineColor: "bg",
      bg: "fg",
      "&[data-show=away], &[data-show=xa]": { bg: "bg", borderWidth: "2.5px", borderColor: "fg" },
      "&[data-show=dnd]": { backgroundImage: "linear-gradient(token(colors.bg), token(colors.bg))", backgroundSize: "60% 2px", backgroundPosition: "center", backgroundRepeat: "no-repeat" },
      "&[data-show=offline]": { bg: "bg", borderWidth: "1.5px", borderStyle: "dashed", borderColor: "fg.muted" },
    },
  },
  variants: {
    size: {
      sm: { root: { w: "7", h: "7" }, fallback: { fontSize: "caption" } },
      md: { root: { w: "10", h: "10" }, fallback: { fontSize: "control" } },
      lg: { root: { w: "16", h: "16" }, fallback: { fontFamily: "display", fontSize: "title" } },
    },
  },
  defaultVariants: { size: "md" },
});

const toast = defineSlotRecipe({
  className: "toast",
  slots: ["root", "title", "description", "actionTrigger", "closeTrigger"],
  base: {
    root: { display: "flex", alignItems: "flex-start", gap: "3", p: "3.5", bg: "fg", color: "bg", borderRadius: "panel", boxShadow: "overlay", minW: "80" },
    title: { fontSize: "body", fontWeight: "semibold" },
    description: { fontSize: "control", opacity: 0.75 },
    actionTrigger: { h: "control.xs", px: "2.5", borderRadius: "tag", bg: "ember.bright", color: "ink", fontWeight: "bold", fontSize: "control" },
    closeTrigger: { opacity: 0.7, _hover: { opacity: 1 } },
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
  // Tailwind v4 also declares `base` and `utilities`. Prefix Panda's layers
  // so the two never merge into one cascade layer while both are installed.
  layers: { reset: "pd_reset", base: "pd_base", tokens: "pd_tokens", recipes: "pd_recipes", utilities: "pd_utilities" },
  conditions: {
    extend: {
      light: "[data-theme=light] &, :root:not([data-theme=dark]) &",
      dark: "[data-theme=dark] &, :root:not([data-theme=light]) &",
      highlighted: "&[data-highlighted]",
      selected: "&[data-selected]",
      checked: "&[data-state=checked]",
      density: "[data-density=compact] &",
    },
  },
  theme: {
    extend: {
      tokens: {
        colors: {
          // Grounds
          paper: { 50: { value: "#fffbf4" }, 100: { value: "#f6efe4" }, 200: { value: "#efe6d6" }, 300: { value: "#e3d9c8" }, 400: { value: "#d8ccb8" } },
          night: { 900: { value: "#16112a" }, 800: { value: "#1c1530" }, 700: { value: "#2a2142" }, 600: { value: "#3a3057" }, 500: { value: "#4a4066" }, 300: { value: "#b3aac6" }, 200: { value: "#d9d1e6" }, 100: { value: "#f3ecdf" } },
          // Ember: the one hot colour
          ember: { 700: { value: "#a3320f" }, 600: { value: "#c43e17" }, 500: { value: "#e4572e" }, 400: { value: "#ff6a3d" }, 300: { value: "#ff8f66" }, 100: { value: "#fbe3d8" } },
          // Gold: recognition
          gold: { 700: { value: "#8a5a0a" }, 500: { value: "#e9b44c" }, 100: { value: "#fdf3dc" }, 900: { value: "#3d3320" } },
          // Moss: resolved / answered
          moss: { 700: { value: "#2f6b3f" }, 500: { value: "#3f7d4e" }, 300: { value: "#7fc98f" }, 100: { value: "#e2efe4" } },
          // Lilac: links in body copy, info
          lilac: { 600: { value: "#5b52c7" }, 300: { value: "#a9a3ea" } },
          // Danger
          red: { 700: { value: "#8e1e17" }, 600: { value: "#b3261e" }, 400: { value: "#e0554b" }, 300: { value: "#ff8f86" } },
          muted: { light: { value: "#5d5670" } },
        },
        fonts: {
          display: { value: "'Fraunces Variable', 'Fraunces', Georgia, serif" },
          body: { value: "'Instrument Sans Variable', 'Instrument Sans', system-ui, sans-serif" },
          mono: { value: "'IBM Plex Mono', ui-monospace, monospace" },
        },
        fontSizes: {
          kicker: { value: "0.6875rem" },
          caption: { value: "0.75rem" },
          control: { value: "0.8125rem" },
          body: { value: "0.875rem" },
          message: { value: "0.9375rem" },
          lead: { value: "1.0625rem" },
          title: { value: "1.375rem" },
          headline: { value: "2.25rem" },
          display: { value: "3.25rem" },
        },
        lineHeights: { tight: { value: "1" }, snug: { value: "1.2" }, body: { value: "1.55" } },
        letterSpacings: { display: { value: "-0.025em" }, title: { value: "-0.01em" }, kicker: { value: "0.08em" } },
        radii: { tag: { value: "0.25rem" }, control: { value: "0.375rem" }, panel: { value: "0.5rem" }, card: { value: "0.625rem" }, hero: { value: "0.75rem" }, pill: { value: "999px" } },
        sizes: {
          "control.xs": { value: "1.75rem" },
          "control.sm": { value: "2rem" },
          "control.md": { value: "2.375rem" },
          "control.lg": { value: "2.625rem" },
          "control.touch": { value: "2.75rem" },
          measure: { value: "68ch" },
          aside: { value: "22.5rem" },
          "aside.wide": { value: "26rem" },
          spaces: { value: "13.75rem" },
        },
        durations: { fast: { value: "140ms" }, normal: { value: "200ms" } },
        easings: { out: { value: "cubic-bezier(0.2, 0.7, 0.2, 1)" } },
        shadows: {
          // The page has no drop shadows. One shadow exists, for overlays.
          overlay: { value: "0 16px 40px rgba(28, 21, 48, 0.16)" },
        },
        borderWidths: { hairline: { value: "1px" }, rule: { value: "1px" } },
        zIndex: { sticky: { value: 10 }, floating: { value: 20 }, popover: { value: 50 }, modal: { value: 60 }, lightbox: { value: 70 } },
      },
      semanticTokens: {
        colors: {
          bg: { value: { _light: "{colors.paper.100}", _dark: "{colors.night.800}" } },
          ink: { value: "{colors.night.800}" },
          fg: {
            DEFAULT: { value: { _light: "{colors.night.800}", _dark: "{colors.night.100}" } },
            muted: { value: { _light: "{colors.muted.light}", _dark: "{colors.night.300}" } },
            soft: { value: { _light: "{colors.night.200}", _dark: "{colors.night.500}" } },
          },
          surface: {
            DEFAULT: { value: { _light: "{colors.paper.50}", _dark: "{colors.night.700}" } },
            2: { value: { _light: "{colors.paper.200}", _dark: "{colors.night.600}" } },
            hover: { value: { _light: "{colors.paper.200}", _dark: "{colors.night.600}" } },
            inverse: { value: { _light: "{colors.night.800}", _dark: "{colors.night.100}" } },
          },
          border: { value: { _light: "{colors.paper.300}", _dark: "{colors.night.500}" } },
          rule: { value: { _light: "{colors.night.800}", _dark: "{colors.night.100}" } },
          ember: {
            DEFAULT: { value: { _light: "{colors.ember.600}", _dark: "{colors.ember.400}" } },
            hover: { value: { _light: "{colors.ember.700}", _dark: "{colors.ember.300}" } },
            fg: { value: { _light: "#ffffff", _dark: "{colors.night.800}" } },
            text: { value: { _light: "{colors.ember.700}", _dark: "{colors.ember.400}" } },
            bright: { value: "{colors.ember.400}" },
            subtle: { value: { _light: "{colors.ember.100}", _dark: "#3d2317" } },
          },
          gold: {
            DEFAULT: { value: "{colors.gold.500}" },
            text: { value: { _light: "{colors.gold.700}", _dark: "{colors.gold.500}" } },
            subtle: { value: { _light: "{colors.gold.100}", _dark: "{colors.gold.900}" } },
          },
          moss: {
            DEFAULT: { value: { _light: "{colors.moss.700}", _dark: "{colors.moss.300}" } },
            fg: { value: { _light: "#ffffff", _dark: "{colors.night.800}" } },
            text: { value: { _light: "{colors.moss.700}", _dark: "{colors.moss.300}" } },
            subtle: { value: { _light: "{colors.moss.100}", _dark: "#1f3a27" } },
          },
          link: { value: { _light: "{colors.lilac.600}", _dark: "{colors.lilac.300}" } },
          danger: {
            DEFAULT: { value: { _light: "{colors.red.600}", _dark: "{colors.red.400}" } },
            hover: { value: { _light: "{colors.red.700}", _dark: "{colors.red.300}" } },
            fg: { value: { _light: "#ffffff", _dark: "{colors.night.800}" } },
            text: { value: { _light: "{colors.red.600}", _dark: "{colors.red.300}" } },
          },
        },
      },
      textStyles: {
        display: { value: { fontFamily: "display", fontSize: "display", fontWeight: "600", lineHeight: "tight", letterSpacing: "display", fontVariationSettings: "'opsz' 144, 'SOFT' 60" } },
        headline: { value: { fontFamily: "display", fontSize: "headline", fontWeight: "600", lineHeight: "tight", letterSpacing: "display", fontVariationSettings: "'opsz' 144, 'SOFT' 60" } },
        title: { value: { fontFamily: "display", fontSize: "title", fontWeight: "500", lineHeight: "snug", letterSpacing: "title", fontVariationSettings: "'opsz' 36, 'SOFT' 30" } },
        lead: { value: { fontFamily: "body", fontSize: "lead", lineHeight: "body" } },
        body: { value: { fontFamily: "body", fontSize: "message", lineHeight: "body" } },
        control: { value: { fontFamily: "body", fontSize: "body", fontWeight: "500", lineHeight: "snug" } },
        kicker: { value: { fontFamily: "mono", fontSize: "kicker", fontWeight: "500", letterSpacing: "kicker", textTransform: "uppercase" } },
        stat: { value: { fontFamily: "display", fontSize: "headline", fontWeight: "600", lineHeight: "tight", fontVariationSettings: "'opsz' 72" } },
      },
      recipes: { button, tag, kicker },
      slotRecipes: { nav, segment, menu, avatar, toast },
    },
  },
  globalCss: {
    html: { colorScheme: "light dark" },
    body: { bg: "bg", color: "fg", fontFamily: "body", fontSize: "body", lineHeight: "body", WebkitFontSmoothing: "antialiased" },
    a: { color: "inherit", textDecoration: "none", _hover: { color: "ember.text" } },
    "p a, li a": { color: "link", textDecoration: "underline", textUnderlineOffset: "2px" },
  },
  staticCss: { recipes: { button: [{ variant: ["*"], size: ["*"] }], tag: [{ tone: ["*"] }] } },
});
