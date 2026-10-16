/**
 * Huddle design tokens: the raw values behind `panda.config.ts`.
 *
 * Kept free of imports so tools other than Panda can read them:
 * `scripts/generate-design-tokens.mjs` emits Android's `WaddleTokens.kt`
 * from this module. Values use Panda's token format: references like
 * `{colors.teal.300}`, the `DEFAULT` key, and the `_dark` (night, the
 * default) and `_light` (daylight) conditions declared in the config.
 */

export const tokens = {
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
    amber: { 500: { value: "#f2b036" }, 700: { value: "#d97706" }, 800: { value: "#8a4d06" } },
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
} as const;

export const semanticTokens = {
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
    warning: {
      DEFAULT: { value: { _dark: "{colors.amber.500}", _light: "{colors.amber.700}" } },
      text: { value: { _dark: "{colors.amber.500}", _light: "{colors.amber.800}" } },
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
} as const;

export const textStyles = {
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
} as const;
