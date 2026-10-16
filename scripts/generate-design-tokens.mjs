#!/usr/bin/env bun
// Design-token generator.
//
// Sources of truth (human-edited):
//   - chat/src/styles/huddle-tokens.ts: the Huddle system's raw Panda
//     tokens, also imported by chat/panda.config.ts. Feeds Android's
//     `WaddleTokens.Huddle`.
//   - chat/src/styles/global/tokens.css: the legacy sheet. Feeds the Apple
//     accent, the website palette and Android's legacy `WaddleTokens`
//     sections.
// Generated outputs (never edit by hand):
//   - apps/apple/Waddle/Assets.xcassets/AccentColor.colorset/Contents.json
//   - website/src/styles/global/brand-palette.css
//   - apps/android/app/src/main/kotlin/social/waddle/android/theme/WaddleTokens.kt
//
// Usage:
//   bun scripts/generate-design-tokens.mjs           # write outputs
//   bun scripts/generate-design-tokens.mjs --check   # exit 1 if outputs are stale
//
// Apple and the website share only the brand accent (chat `--primary`).
// Android takes the whole semantic sheet — colours in both schemes, the
// spacing, radius and type scales — because it has no platform palette
// worth keeping: Material 3's defaults are lavender and pink, not neutral
// like Apple's system greys. Android maps these tokens onto Material
// roles in WaddleTheme.kt; the mapping is the design decision, the values
// stay here.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  semanticTokens as huddleSemanticTokens,
  textStyles as huddleTextStyles,
  tokens as huddleTokens,
} from "../chat/src/styles/huddle-tokens.ts";
import { createPandaTokenResolver } from "./design-tokens/panda-tokens.mjs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

const TOKENS_CSS = join(repoRoot, "chat/src/styles/global/tokens.css");
const ACCENT_COLORSET = join(
  repoRoot,
  "apps/apple/Waddle/Assets.xcassets/AccentColor.colorset/Contents.json",
);
const WEBSITE_PALETTE = join(
  repoRoot,
  "website/src/styles/global/brand-palette.css",
);
const ANDROID_TOKENS = join(
  repoRoot,
  "apps/android/app/src/main/kotlin/social/waddle/android/theme/WaddleTokens.kt",
);

// ---------------------------------------------------------------------------
// oklch -> sRGB (Björn Ottosson's OKLab reference transform)
// ---------------------------------------------------------------------------

function oklchToSrgb({ l, c, h }) {
  const hRad = (h * Math.PI) / 180;
  const a = c * Math.cos(hRad);
  const b = c * Math.sin(hRad);

  const lp = l + 0.3963377774 * a + 0.2158037573 * b;
  const mp = l - 0.1055613458 * a - 0.0638541728 * b;
  const sp = l - 0.0894841775 * a - 1.291485548 * b;

  const l3 = lp ** 3;
  const m3 = mp ** 3;
  const s3 = sp ** 3;

  const linear = [
    4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
    -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
    -0.0041960863 * l3 - 0.7034186147 * m3 + 1.707614701 * s3,
  ];

  return linear.map((v) => {
    const clamped = Math.min(1, Math.max(0, v));
    const srgb =
      clamped <= 0.0031308
        ? 12.92 * clamped
        : 1.055 * clamped ** (1 / 2.4) - 0.055;
    return Math.min(1, Math.max(0, srgb));
  });
}

function srgbToHex(rgb) {
  return (
    "#" +
    rgb
      .map((v) =>
        Math.round(v * 255)
          .toString(16)
          .padStart(2, "0"),
      )
      .join("")
  );
}

// ---------------------------------------------------------------------------
// Parse the chat token sheet
// ---------------------------------------------------------------------------

function parseLightDarkOklch(css, varName) {
  const pattern = new RegExp(
    `--${varName}:\\s*light-dark\\(\\s*oklch\\(([^)]+)\\)\\s*,\\s*oklch\\(([^)]+)\\)\\s*\\)`,
  );
  const match = css.match(pattern);
  if (!match) {
    throw new Error(`--${varName}: light-dark(oklch(...), oklch(...)) not found in tokens.css`);
  }
  const parse = (body) => {
    const parts = body.trim().split(/[\s/]+/).map(Number);
    if (parts.length < 3 || parts.some(Number.isNaN)) {
      throw new Error(`--${varName}: unparseable oklch body "${body}"`);
    }
    return { l: parts[0], c: parts[1], h: parts[2] };
  };
  return { light: parse(match[1]), dark: parse(match[2]) };
}

const tokensCss = readFileSync(TOKENS_CSS, "utf8");
const primary = parseLightDarkOklch(tokensCss, "primary");

// Plain colour values: `#rrggbb`, `rgba(r, g, b, a)`, or `light-dark(x, y)`
// of those. `var(--other)` resolves through the sheet. Returns
// { light, dark } as 0xAARRGGBB strings.
function colorLiteral(value) {
  const hex = value.match(/^#([0-9a-f]{6})$/i);
  if (hex) return `0xFF${hex[1].toUpperCase()}`;
  const rgba = value.match(/^rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([0-9.]+)\s*\)$/);
  if (rgba) {
    const [r, g, b] = rgba.slice(1, 4).map((v) => Number(v).toString(16).padStart(2, "0"));
    const a = Math.round(Number(rgba[4]) * 255).toString(16).padStart(2, "0");
    return `0x${a}${r}${g}${b}`.toUpperCase().replace("0X", "0x");
  }
  const oklch = value.match(/^oklch\(([^)]+)\)$/);
  if (oklch) {
    const [l, c, h] = oklch[1].trim().split(/\s+/).map(Number);
    return `0xFF${srgbToHex(oklchToSrgb({ l, c, h })).slice(1).toUpperCase()}`;
  }
  return null;
}

function splitLightDark(body) {
  let depth = 0;
  for (let i = 0; i < body.length; i++) {
    if (body[i] === "(") depth++;
    else if (body[i] === ")") depth--;
    else if (body[i] === "," && depth === 0) {
      return [body.slice(0, i).trim(), body.slice(i + 1).trim()];
    }
  }
  throw new Error(`light-dark() without two arguments: ${body}`);
}

function declarations(css) {
  const root = css.slice(css.indexOf(":root {"), css.indexOf("\n}\n"));
  const decls = new Map();
  for (const match of root.matchAll(/^\s*--([a-z0-9-]+):\s*([^;]+);/gm)) {
    decls.set(match[1], match[2].trim());
  }
  return decls;
}

const tokenDecls = declarations(tokensCss);

function resolveColor(name, seen = new Set()) {
  if (seen.has(name)) throw new Error(`--${name}: var() cycle`);
  seen.add(name);
  const raw = tokenDecls.get(name);
  if (raw === undefined) throw new Error(`--${name} not found in tokens.css`);
  const ref = raw.match(/^var\(--([a-z0-9-]+)\)$/);
  if (ref) return resolveColor(ref[1], seen);
  const ld = raw.match(/^light-dark\((.*)\)$/);
  if (ld) {
    const [l, d] = splitLightDark(ld[1]).map((v) => {
      const inner = v.match(/^var\(--([a-z0-9-]+)\)$/);
      return inner ? null : colorLiteral(v);
    });
    if (!l || !d) throw new Error(`--${name}: unsupported light-dark() operand in "${raw}"`);
    return { light: l, dark: d };
  }
  const literal = colorLiteral(raw);
  if (!literal) throw new Error(`--${name}: unsupported colour "${raw}"`);
  return { light: literal, dark: literal };
}

function remToDp(name) {
  const raw = tokenDecls.get(name);
  const rem = raw?.match(/^([0-9.]+)rem$/);
  if (!rem) throw new Error(`--${name}: expected a rem length, got "${raw}"`);
  return Number(rem[1]) * 16;
}

function plainNumber(name) {
  const raw = tokenDecls.get(name);
  if (raw === undefined || Number.isNaN(Number(raw))) {
    throw new Error(`--${name}: expected a number, got "${raw}"`);
  }
  return Number(raw);
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

function colorsetJson({ light, dark }) {
  const component = (v) => v.toFixed(3);
  const entry = (rgb, appearance) => ({
    ...(appearance
      ? {
          appearances: [
            { appearance: "luminosity", value: appearance },
          ],
        }
      : {}),
    color: {
      "color-space": "srgb",
      components: {
        alpha: "1.000",
        blue: component(rgb[2]),
        green: component(rgb[1]),
        red: component(rgb[0]),
      },
    },
    idiom: "universal",
  });
  return (
    JSON.stringify(
      {
        colors: [entry(oklchToSrgb(light)), entry(oklchToSrgb(dark), "dark")],
        info: { author: "scripts/generate-design-tokens.mjs", version: 1 },
      },
      null,
      2,
    ) + "\n"
  );
}

function websitePaletteCss({ light, dark }) {
  const lightHex = srgbToHex(oklchToSrgb(light));
  const darkHex = srgbToHex(oklchToSrgb(dark));
  return `/* GENERATED — do not edit.
 * Source: chat/src/styles/global/tokens.css (--primary)
 * Regenerate: bun scripts/generate-design-tokens.mjs
 */
:root {
  --brand-accent: oklch(${light.l} ${light.c} ${light.h});
  --brand-accent-dark: oklch(${dark.l} ${dark.c} ${dark.h});
  --brand-accent-hex: ${lightHex};
  --brand-accent-dark-hex: ${darkHex};
}
`;
}

// CSS token name -> Kotlin property name (Android consumes these).
const ANDROID_COLORS = [
  "background",
  "foreground",
  "card",
  "primary",
  "primary-foreground",
  "secondary",
  "muted",
  "muted-foreground",
  "destructive",
  "destructive-foreground",
  "destructive-text",
  "border",
  "sidebar",
  "sidebar-accent",
  "surface",
  "surface-strong",
  "rail",
  "rail-foreground",
  "rail-hover",
  "success",
  "warning",
  "warning-foreground",
  "live",
  "live-foreground",
  "live-text",
  "glow",
  "gold",
  "gold-text",
  "moss",
  "rich-code-bg",
  "rich-code-border",
  "rich-code-foreground",
  "rich-quote-border",
  "rich-quote-foreground",
  "rich-link",
  "night-ground",
  "night-foreground",
];
const ANDROID_SPACES = ["2xs", "xs", "sm", "md", "lg", "xl", "2xl", "3xl"];
const ANDROID_RADII = ["xs", "sm", "md", "lg", "xl"];
const ANDROID_TEXT = ["caption", "meta", "control", "body", "message", "title", "display"];
const ANDROID_LEADING = ["compact", "control", "body", "message"];

const camel = (name) =>
  name.replace(/^([0-9])/, "x$1").replace(/-([a-z0-9])/g, (_, c) => c.toUpperCase());

// ---------------------------------------------------------------------------
// Huddle (chat/src/styles/huddle-tokens.ts) -> WaddleTokens.Huddle
// ---------------------------------------------------------------------------

// panda.config.ts: night is the default (`_dark`), daylight is `_light`.
const HUDDLE_SCHEMES = { night: "_dark", daylight: "_light" };

const huddle = createPandaTokenResolver({
  tokens: huddleTokens,
  semanticTokens: huddleSemanticTokens,
  conditions: HUDDLE_SCHEMES,
});

// `bg.rail` -> `bgRail`, `surface.2` -> `surface2`.
function kotlinName(dotted) {
  const name = dotted
    .split(".")
    .map((part, i) => (i === 0 ? part : part.charAt(0).toUpperCase() + part.slice(1)))
    .join("");
  if (!/^[a-z][A-Za-z0-9]*$/.test(name)) {
    throw new Error(`token "${dotted}" has no valid Kotlin name (got "${name}")`);
  }
  return name;
}

const pascal = (name) => kotlinName(name).replace(/^./, (c) => c.toUpperCase());

function huddleValue(category, name, scheme) {
  return huddle.resolve(category, name, scheme);
}

function huddleColor(name, scheme) {
  const value = huddleValue("colors", name, scheme);
  const literal = typeof value === "string" ? colorLiteral(value.trim()) : null;
  if (!literal) throw new Error(`colors.${name}: unsupported colour "${value}"`);
  return literal;
}

// rem (16px root) or px -> a dp/sp number.
function huddleLength(category, name) {
  const value = String(huddleValue(category, name));
  const match = value.match(/^(-?[0-9.]+)(rem|px)$/);
  if (!match) throw new Error(`${category}.${name}: expected rem or px, got "${value}"`);
  return Number(match[1]) * (match[2] === "rem" ? 16 : 1);
}

// Kotlin numeric literal for an `.em`/`.dp` receiver: negatives need parens.
const ktNumber = (n) => (n < 0 ? `(${n})` : `${n}`);
const ktFloat = (n) => `${n}f`;

function huddleEm(category, name) {
  const value = String(huddleValue(category, name));
  const match = value.match(/^(-?[0-9.]+)(em)?$/);
  if (!match) throw new Error(`${category}.${name}: expected a number or em, got "${value}"`);
  return `${ktNumber(Number(match[1]))}.em`;
}

function huddleMillis(name) {
  const value = String(huddleValue("durations", name));
  const match = value.match(/^([0-9]+)ms$/);
  if (!match) throw new Error(`durations.${name}: expected ms, got "${value}"`);
  return Number(match[1]);
}

function huddleCubicBezier(name) {
  const value = String(huddleValue("easings", name));
  const match = value.match(/^cubic-bezier\(([^)]+)\)$/);
  const points = match?.[1].split(",").map((v) => Number(v.trim()));
  if (!points || points.length !== 4 || points.some(Number.isNaN)) {
    throw new Error(`easings.${name}: expected cubic-bezier(x1, y1, x2, y2), got "${value}"`);
  }
  return `CubicBezierEasing(${points.map(ktFloat).join(", ")})`;
}

// Single-layer box-shadow: `x y blur [spread] color`.
function huddleShadow(name) {
  const value = String(huddleValue("shadows", name));
  const match = value.match(/^(-?[0-9.]+(?:px)?)\s+(-?[0-9.]+(?:px)?)\s+([0-9.]+(?:px)?)(?:\s+(-?[0-9.]+(?:px)?))?\s+(.+)$/);
  const color = match && colorLiteral(match[5].trim());
  if (!color) throw new Error(`shadows.${name}: expected "x y blur [spread] colour", got "${value}"`);
  const dp = (v) => `${ktNumber(Number.parseFloat(v ?? "0"))}.dp`;
  return [
    `offsetX = ${dp(match[1])}`,
    `offsetY = ${dp(match[2])}`,
    `blur = ${dp(match[3])}`,
    `spread = ${dp(match[4])}`,
    `color = Color(${color})`,
  ];
}

// `val name = Type(` + one named argument per line + `)`.
const ktConstruct = (name, type, args) => [
  `val ${kotlinName(name)} = ${type}(`,
  ...args.map((arg) => `    ${arg},`),
  ")",
];

const FONT_WEIGHTS = { 400: "Normal", 500: "Medium", 600: "SemiBold", 700: "Bold" };

function huddleTextStyle(name, style) {
  const known = new Set(["fontFamily", "fontSize", "fontWeight", "lineHeight", "letterSpacing", "textTransform"]);
  for (const key of Object.keys(style)) {
    if (!known.has(key)) throw new Error(`textStyles.${name}: unsupported property "${key}"`);
  }
  if (!huddle.tokenNames("fonts").includes(style.fontFamily)) {
    throw new Error(`textStyles.${name}: unknown font "${style.fontFamily}"`);
  }
  const weight = FONT_WEIGHTS[style.fontWeight ?? "400"];
  if (!weight) throw new Error(`textStyles.${name}: unsupported fontWeight "${style.fontWeight}"`);
  if (style.textTransform !== undefined && style.textTransform !== "uppercase") {
    throw new Error(`textStyles.${name}: unsupported textTransform "${style.textTransform}"`);
  }
  return [
    `font = HuddleFont.${pascal(style.fontFamily)}`,
    `size = ${huddleLength("fontSizes", style.fontSize)}.sp`,
    `weight = FontWeight.${weight}`,
    `lineHeight = ${style.lineHeight ? huddleEm("lineHeights", style.lineHeight) : "TextUnit.Unspecified"}`,
    `letterSpacing = ${style.letterSpacing ? huddleEm("letterSpacings", style.letterSpacing) : "0.em"}`,
    `uppercase = ${style.textTransform === "uppercase"}`,
  ];
}

function huddleKt() {
  const indent = (lines, depth) => lines.map((l) => `${"    ".repeat(depth)}${l}`).join("\n");
  const scheme = (key) =>
    indent(
      huddle.semanticNames("colors").map((n) => `val ${kotlinName(n)} = Color(${huddleColor(n, key)})`),
      3,
    );
  const dims = (category, names, unit, rename = (n) => n) =>
    indent(names.map((n) => `val ${kotlinName(rename(n))} = ${ktNumber(huddleLength(category, n))}.${unit}`), 3);
  const controls = huddle.tokenNames("sizes").filter((n) => n.startsWith("control."));
  const textStyles = Object.entries(huddleTextStyles).flatMap(([name, { value }]) =>
    ktConstruct(name, "HuddleTextStyle", huddleTextStyle(name, value)),
  );
  const shadows = huddle
    .tokenNames("shadows")
    .flatMap((name) => ktConstruct(name, "HuddleShadow", huddleShadow(name)));
  return `    /**
     * Huddle, the web's design system (chat/panda.config.ts). Night is the
     * default scheme; Daylight is the light one.
     */
    object Huddle {
        /** Night scheme (the default). */
        object Night {
${scheme("night")}
        }

        /** Daylight scheme. */
        object Daylight {
${scheme("daylight")}
        }

        object Radius {
${dims("radii", huddle.tokenNames("radii"), "dp")}
        }

        /** Avatar fallback tints (web \`avatarTints\`), one picked per name; same in both schemes. */
        val avatarTints = listOf(
${indent(huddle.tokenNames("colors").filter((n) => n.startsWith("avatarTints.")).map((n) => `Color(${huddleColor(n, "night")}),`), 3)}
        )

        /** Control heights (web \`sizes.control.*\`). */
        object ControlHeight {
${dims("sizes", controls, "dp", (n) => n.slice("control.".length))}
        }

        object FontSize {
${dims("fontSizes", huddle.tokenNames("fontSizes"), "sp")}
        }

        /** Line height relative to the font size. */
        object LineHeight {
${indent(huddle.tokenNames("lineHeights").map((n) => `val ${kotlinName(n)} = ${huddleEm("lineHeights", n)}`), 3)}
        }

        object LetterSpacing {
${indent(huddle.tokenNames("letterSpacings").map((n) => `val ${kotlinName(n)} = ${huddleEm("letterSpacings", n)}`), 3)}
        }

        /** Web \`textStyles\`; the theme maps [HuddleFont] onto font families. */
        object Type {
${indent(textStyles, 3)}
        }

        object DurationMillis {
${indent(huddle.tokenNames("durations").map((n) => `const val ${kotlinName(n).toUpperCase()} = ${huddleMillis(n)}`), 3)}
        }

        object Easing {
${indent(huddle.tokenNames("easings").map((n) => `val ${kotlinName(n)} = ${huddleCubicBezier(n)}`), 3)}
        }

        object Shadow {
${indent(shadows, 3)}
        }
    }`;
}

function huddleFontsKt() {
  const entries = huddle.tokenNames("fonts").map((n) => {
    const family = String(huddleValue("fonts", n)).split(",")[0].trim().replace(/^'|'$/g, "");
    return `    /** ${family} */\n    ${pascal(n)},`;
  });
  return `/** Huddle font roles (web \`fonts\` tokens). */
enum class HuddleFont {
${entries.join("\n\n")}
}

/**
 * A Huddle text style. [lineHeight] and [letterSpacing] are in em;
 * [lineHeight] is unspecified where the web inherits it.
 */
data class HuddleTextStyle(
    val font: HuddleFont,
    val size: TextUnit,
    val weight: FontWeight,
    val lineHeight: TextUnit,
    val letterSpacing: TextUnit,
    val uppercase: Boolean,
)

/** A single-layer box shadow. */
data class HuddleShadow(
    val offsetX: Dp,
    val offsetY: Dp,
    val blur: Dp,
    val spread: Dp,
    val color: Color,
)`;
}

function androidTokensKt() {
  const colors = ANDROID_COLORS.map((name) => [camel(name), resolveColor(name)]);
  const scheme = (pick) =>
    colors.map(([prop, value]) => `        val ${prop} = Color(${value[pick]})`).join("\n");
  const dims = (names, prefix, unit) =>
    names
      .map((n) => `        val ${camel(n === "2xs" ? "xxs" : n === "2xl" ? "xxl" : n === "3xl" ? "xxxl" : n)} = ${remToDp(`${prefix}-${n}`)}.${unit}`)
      .join("\n");
  return `// GENERATED — do not edit.
// Sources: chat/src/styles/huddle-tokens.ts (Huddle),
//          chat/src/styles/global/tokens.css (everything else)
// Regenerate: bun scripts/generate-design-tokens.mjs
package social.waddle.android.theme

import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/** Waddle's semantic design tokens, shared with the web client. */
object WaddleTokens {
    /** Daylight scheme. */
    object Light {
${scheme("light")}
    }

    /** Night scheme. */
    object Dark {
${scheme("dark")}
    }

    object Space {
${dims(ANDROID_SPACES, "space", "dp")}
    }

    object Radius {
${dims(ANDROID_RADII, "radius", "dp")}
    }

    object Text {
${dims(ANDROID_TEXT, "text", "sp")}
    }

    /** Line height as a multiple of the font size. */
    object Leading {
${ANDROID_LEADING.map((n) => `        const val ${camel(n).toUpperCase()} = ${plainNumber(`leading-${n}`)}f`).join("\n")}
    }

${huddleKt()}
}

${huddleFontsKt()}
`;
}

const outputs = [
  { path: ACCENT_COLORSET, content: colorsetJson(primary) },
  { path: WEBSITE_PALETTE, content: websitePaletteCss(primary) },
  { path: ANDROID_TOKENS, content: androidTokensKt() },
];

const checkMode = process.argv.includes("--check");
let stale = false;

for (const { path, content } of outputs) {
  let current = null;
  try {
    current = readFileSync(path, "utf8");
  } catch {
    // missing file counts as stale
  }
  if (current === content) {
    continue;
  }
  if (checkMode) {
    stale = true;
    console.error(`stale: ${path}`);
  } else {
    writeFileSync(path, content);
    console.log(`wrote: ${path}`);
  }
}

if (checkMode) {
  if (stale) {
    console.error(
      "Design tokens out of sync. Run: bun scripts/generate-design-tokens.mjs",
    );
    process.exit(1);
  }
  console.log("design tokens in sync");
}
