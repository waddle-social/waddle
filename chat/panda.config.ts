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
// Token values live in their own import-free module so the native token
// generator (scripts/generate-design-tokens.mjs) can read them too.
import { semanticTokens, textStyles, tokens } from "./src/styles/huddle-tokens";

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
  description: "Room, event and question cards. `live` gets the ember border and glow; `warning` (a ringing call) and `active` (a syncing one) are outlined in their state colour without a glow.",
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
      active: { root: { borderColor: "action/40" }, kicker: { color: "action.text" } },
      warning: { root: { borderColor: "warning/40" }, kicker: { color: "warning.text" } },
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
      tokens,
      semanticTokens,
      textStyles,
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
