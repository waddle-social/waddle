#!/usr/bin/env bun
// Design-token generator.
//
// Sources of truth (human-edited):
//   - design/src/preset.ts: the Polar Huddle preset (@waddle/design). Feeds
//     Apple's `WaddleTokens` and accent, and Android's `WaddleTokens.Huddle`.
//   - scripts/design-tokens/legacy-tokens.css: the legacy chat sheet, a
//     frozen copy. Feeds Android's legacy `WaddleTokens` sections until
//     those move to the preset too.
// Generated outputs (never edit by hand):
//   - apps/apple/Waddle/Assets.xcassets/AccentColor.colorset/Contents.json
//   - apps/apple/Waddle/Design/WaddleTokens.swift
//   - apps/android/app/src/main/kotlin/social/waddle/android/theme/WaddleTokens.kt
//
// Usage:
//   bun scripts/generate-design-tokens.mjs           # write outputs
//   bun scripts/generate-design-tokens.mjs --check   # exit 1 if outputs are stale
//
// Apple and Android both take the preset's semantic colours in both
// schemes, plus the radius, type and motion scales; Apple also takes
// spacing and border widths. Elevation, state layers and springs are
// derived on each platform (Theme.swift, WaddleTheme.kt): the mapping is
// the design decision, the values stay here.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { waddlePreset } from "../design/src/preset.ts";
import { createPandaTokenResolver } from "./design-tokens/panda-tokens.mjs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

const TOKENS_CSS = join(repoRoot, "scripts/design-tokens/legacy-tokens.css");
const ACCENT_COLORSET = join(
  repoRoot,
  "apps/apple/Waddle/Assets.xcassets/AccentColor.colorset/Contents.json",
);
const APPLE_TOKENS = join(repoRoot, "apps/apple/Waddle/Design/WaddleTokens.swift");
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

const tokensCss = readFileSync(TOKENS_CSS, "utf8");

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

// `light` and `dark` are sRGB component triples in 0..1.
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
        colors: [entry(light), entry(dark, "dark")],
        info: { author: "scripts/generate-design-tokens.mjs", version: 1 },
      },
      null,
      2,
    ) + "\n"
  );
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
// Polar Huddle (design/src/preset.ts) -> WaddleTokens.Huddle
// ---------------------------------------------------------------------------

const presetTheme = waddlePreset.theme.extend;

// The preset's `t()` values key night as `_night` (and `base`), daylight as `_daylight`.
const HUDDLE_SCHEMES = { night: "_night", daylight: "_daylight" };

const huddle = createPandaTokenResolver({
  tokens: presetTheme.tokens,
  semanticTokens: presetTheme.semanticTokens,
  conditions: HUDDLE_SCHEMES,
});

// Semantic colours built from `color-mix()` or `transparent`: Compose
// derives these itself (WaddleTheme.kt), so they are not emitted.
const DERIVED_COLOR_GROUPS = ["state.", "material", "elev."];

// Text styles with fluid (clamp) sizes: web-only.
const FLUID_TEXT_STYLES = new Set(["lead", "marketing"]);

// `bg.rail` -> `bgRail`, `surface.2` -> `surface2`, `2xs` -> `xxs`, `4xl` -> `xxxxl`.
function kotlinName(dotted) {
  const name = dotted
    .split(".")
    .map((part) => part.replace(/^(\d)(x[sl])$/, (_, n, rest) => "x".repeat(Number(n) - 1) + rest))
    .map((part, i) => (i === 0 ? part : part.charAt(0).toUpperCase() + part.slice(1)))
    .join("");
  if (!/^[a-z][A-Za-z0-9]*$/.test(name)) {
    throw new Error(`token "${dotted}" has no valid Kotlin name (got "${name}")`);
  }
  return name;
}

// Token names that are Kotlin keywords (`easings.in`) get a readable name instead.
const KT_KEYWORD_NAMES = { in: "easeIn" };
const ktIdentifier = (name) => KT_KEYWORD_NAMES[name] ?? name;

const pascal = (name) => kotlinName(name).replace(/^./, (c) => c.toUpperCase());

function huddleValue(category, name, scheme = "night") {
  return huddle.resolve(category, name, scheme);
}

function huddleColor(name, scheme) {
  const value = huddleValue("colors", name, scheme);
  const literal = typeof value === "string" ? colorLiteral(value.trim()) : null;
  if (!literal) throw new Error(`colors.${name}: unsupported colour "${value}"`);
  return literal;
}

function px(label, value) {
  const match = /^(-?[\d.]+)px$/.exec(String(value));
  if (!match) throw new Error(`${label}: expected px, got "${value}"`);
  return Number(match[1]);
}

const isPx = (value) => /^-?[\d.]+px$/.test(String(value));

// Kotlin numeric literal for an `.em`/`.dp` receiver: negatives need parens.
const ktNumber = (n) => (n < 0 ? `(${n})` : `${n}`);
const ktFloat = (n) => `${n}f`;
const round3 = (n) => Math.round(n * 1000) / 1000;

function huddleMillis(name) {
  const value = String(huddleValue("durations", name));
  const match = /^(\d+)ms$/.exec(value);
  if (!match) throw new Error(`durations.${name}: expected ms, got "${value}"`);
  return Number(match[1]);
}

function huddleCubicBezier(name) {
  const value = String(huddleValue("easings", name));
  const match = /^cubic-bezier\(([^)]+)\)$/.exec(value);
  const points = match?.[1].split(",").map((v) => Number(v.trim()));
  if (points?.length !== 4 || points.some(Number.isNaN)) {
    throw new Error(`easings.${name}: expected cubic-bezier(x1, y1, x2, y2), got "${value}"`);
  }
  return points;
}

// Semantic colours both platforms take; the derived groups stay out.
const huddleColorNames = () =>
  huddle
    .semanticNames("colors")
    .filter((n) => !DERIVED_COLOR_GROUPS.some((g) => n === g || n.startsWith(g)));

const isAvatarTint = (name) => /^avatar\.\d+$/.test(name);

// Easings a platform curve can express: `linear()` springs are web-only.
const huddleBezierEasings = () =>
  huddle
    .tokenNames("easings")
    .filter((n) => !String(huddleValue("easings", n)).startsWith("linear("));

// Text styles with fixed sizes, as `[name, { font, size, weight, leading, tracking }]`.
const huddleTextStyles = () =>
  Object.entries(presetTheme.textStyles)
    .filter(([name]) => !FLUID_TEXT_STYLES.has(name))
    .map(([name, { value }]) => [name, huddleTextStyle(name, value)]);

// `val name = Type(` + one named argument per line + `)`.
const ktConstruct = (name, type, args) => [
  `val ${kotlinName(name)} = ${type}(`,
  ...args.map((arg) => `    ${arg},`),
  ")",
];

// Web-only text style properties (wrapping, numerals, colour, per-theme or
// per-pointer tweaks, width axis): Compose styles do without them.
const WEB_ONLY_TEXT = new Set(["textWrap", "fontVariantNumeric", "color", "_night", "_coarse", "fontStretch"]);

// A token name or a raw value: `md`, `20px`, `1.5`, `0.01em`.
function huddleTextStyle(name, style) {
  const known = new Set(["fontFamily", "fontSize", "fontWeight", "lineHeight", "letterSpacing"]);
  for (const key of Object.keys(style)) {
    if (!known.has(key) && !WEB_ONLY_TEXT.has(key)) {
      throw new Error(`textStyles.${name}: unsupported property "${key}"`);
    }
  }
  if (!huddle.tokenNames("fonts").includes(style.fontFamily)) {
    throw new Error(`textStyles.${name}: unknown font "${style.fontFamily}"`);
  }
  const size = px(`textStyles.${name}.fontSize`, huddleValue("fontSizes", style.fontSize));
  const lh = huddle.tokenNames("lineHeights").includes(style.lineHeight)
    ? huddleValue("lineHeights", style.lineHeight)
    : style.lineHeight;
  const leading = isPx(lh) ? px(`textStyles.${name}.lineHeight`, lh) / size : Number(lh);
  if (Number.isNaN(leading)) throw new Error(`textStyles.${name}: unsupported lineHeight "${lh}"`);
  const ls = style.letterSpacing === undefined
    ? "0em"
    : huddle.tokenNames("letterSpacings").includes(style.letterSpacing)
      ? huddleValue("letterSpacings", style.letterSpacing)
      : style.letterSpacing;
  const em = /^(-?[\d.]+)em$/.exec(String(ls));
  if (!em) throw new Error(`textStyles.${name}: unsupported letterSpacing "${ls}"`);
  if (!Number.isInteger(style.fontWeight)) {
    throw new Error(`textStyles.${name}: unsupported fontWeight "${style.fontWeight}"`);
  }
  return {
    font: style.fontFamily,
    size,
    weight: style.fontWeight,
    leading: round3(leading),
    tracking: Number(em[1]),
  };
}

const ktTextStyle = ({ font, size, weight, leading, tracking }) => [
  `font = HuddleFont.${pascal(font)}`,
  `size = ${size}.sp`,
  `weight = FontWeight(${weight})`,
  `lineHeight = ${leading}.em`,
  `letterSpacing = ${ktNumber(tracking)}.em`,
];

// Control heights from the comfortable density (`sizes.chipH`, `rowH`,
// `controlH`, `target`); xs is the compact chip.
function controlHeights() {
  const sizes = presetTheme.semanticTokens.sizes;
  return {
    xs: px("sizes.chipH._compact", sizes.chipH.value._compact.base),
    sm: px("sizes.chipH", sizes.chipH.value.base),
    md: px("sizes.rowH", sizes.rowH.value.base),
    lg: px("sizes.controlH", sizes.controlH.value.base),
    touch: px("sizes.target", huddleValue("sizes", "target")),
  };
}

function huddleKt() {
  const indent = (lines, depth) => lines.map((l) => `${"    ".repeat(depth)}${l}`).join("\n");
  const colorNames = huddleColorNames();
  const scheme = (key) =>
    indent(colorNames.map((n) => `val ${kotlinName(n)} = Color(${huddleColor(n, key)})`), 3);
  const pxDims = (category, unit) =>
    indent(
      huddle
        .tokenNames(category)
        .filter((n) => isPx(huddleValue(category, n)))
        .map((n) => `val ${kotlinName(n)} = ${ktNumber(px(`${category}.${n}`, huddleValue(category, n)))}.${unit}`),
      3,
    );
  const avatarTints = colorNames.filter(isAvatarTint);
  const textStyles = huddleTextStyles().flatMap(([name, style]) =>
    ktConstruct(name, "HuddleTextStyle", ktTextStyle(style)),
  );
  const easings = huddleBezierEasings();
  return `    /**
     * Polar Huddle (design/src/preset.ts). Night is the default scheme;
     * Daylight is the light one.
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
${pxDims("radii", "dp")}
        }

        /** Avatar fallback tints (web \`avatar.N\`), one picked per name; same in both schemes. */
        val avatarTints = listOf(
${indent(avatarTints.map((n) => `Color(${huddleColor(n, "night")}),`), 3)}
        )

        /** Control heights (comfortable density; touch is the 44dp target). */
        object ControlHeight {
${indent(Object.entries(controlHeights()).map(([n, v]) => `val ${n} = ${v}.dp`), 3)}
        }

        object FontSize {
${pxDims("fontSizes", "sp")}
        }

        /** Web \`textStyles\`; the theme maps [HuddleFont] onto font families. */
        object Type {
${indent(textStyles, 3)}
        }

        object DurationMillis {
${indent(huddle.tokenNames("durations").map((n) => `const val ${kotlinName(n).toUpperCase()} = ${huddleMillis(n)}`), 3)}
        }

        object Easing {
${indent(easings.map((n) => `val ${ktIdentifier(kotlinName(n))} = CubicBezierEasing(${huddleCubicBezier(n).map(ktFloat).join(", ")})`), 3)}
        }
    }`;
}

function huddleFontsKt() {
  const entries = huddle.tokenNames("fonts").map((n) => `    ${pascal(n)},`);
  return `/** Polar Huddle font roles (web \`fonts\` tokens). */
enum class HuddleFont {
${entries.join("\n")}
}

/** A Polar Huddle text style. [lineHeight] and [letterSpacing] are in em. */
data class HuddleTextStyle(
    val font: HuddleFont,
    val size: TextUnit,
    val weight: FontWeight,
    val lineHeight: TextUnit,
    val letterSpacing: TextUnit,
)`;
}

// ---------------------------------------------------------------------------
// Polar Huddle (design/src/preset.ts) -> apps/apple WaddleTokens.swift
// ---------------------------------------------------------------------------

// Token names that are Swift keywords get a readable name instead.
const SWIFT_KEYWORD_NAMES = { in: "easeIn" };
const swiftName = (dotted) => SWIFT_KEYWORD_NAMES[kotlinName(dotted)] ?? kotlinName(dotted);

// `0xAARRGGBB` -> sRGB component triple in 0..1 (alpha dropped).
const literalRgb = (literal) =>
  [2, 4, 6].map((i) => Number.parseInt(literal.slice(2 + i, 4 + i), 16) / 255);

// Spacing steps are numbers on the web (`2`, `0.5`); Swift names them `s2`, `s0_5`.
const swiftSpacingName = (step) => `s${String(step).replace(".", "_")}`;

function appleTokensSwift() {
  const indent = (lines, depth) => lines.map((l) => `${"    ".repeat(depth)}${l}`).join("\n");
  const swiftColor = (n) =>
    `Color(night: ${huddleColor(n, "night")}, daylight: ${huddleColor(n, "daylight")})`;
  const colorNames = huddleColorNames();
  const colors = colorNames
    .filter((n) => !isAvatarTint(n))
    .map((n) => `static let ${swiftName(n)} = ${swiftColor(n)}`);
  const avatarTints = colorNames.filter(isAvatarTint).map((n) => `${swiftColor(n)},`);
  const pxDims = (category) =>
    huddle
      .tokenNames(category)
      .filter((n) => isPx(huddleValue(category, n)))
      .map((n) => `static let ${swiftName(n)}: CGFloat = ${px(`${category}.${n}`, huddleValue(category, n))}`);
  // JS lists integer-like keys first (`1`, `2`, … then `0.5`): order the grid by size.
  const spacing = huddle
    .tokenNames("spacing")
    .map((n) => [n, px(`spacing.${n}`, huddleValue("spacing", n))])
    .sort(([, a], [, b]) => a - b)
    .map(([n, value]) => `static let ${swiftSpacingName(n)}: CGFloat = ${value}`);
  const textStyles = huddleTextStyles().map(
    ([name, { font, size, weight, leading, tracking }]) =>
      `static let ${swiftName(name)} = HuddleTextStyle(font: .${swiftName(font)}, size: ${size}, weight: ${weight}, lineHeight: ${leading}, letterSpacing: ${tracking})`,
  );
  const durations = huddle
    .tokenNames("durations")
    .map((n) => `static let ${swiftName(n)}: TimeInterval = ${huddleMillis(n) / 1000}`);
  const easings = huddleBezierEasings().map((n) => {
    const [x1, y1, x2, y2] = huddleCubicBezier(n);
    return `static let ${swiftName(n)} = UnitCurve.bezier(startControlPoint: UnitPoint(x: ${x1}, y: ${y1}), endControlPoint: UnitPoint(x: ${x2}, y: ${y2}))`;
  });
  return `// GENERATED — do not edit.
// Source: design/src/preset.ts (Polar Huddle)
// Regenerate: bun scripts/generate-design-tokens.mjs
import SwiftUI

/// Polar Huddle design tokens, shared with the web and Android clients.
enum WaddleTokens {
    /// Semantic colours. Each resolves to its Night value in a dark
    /// appearance and its Daylight value in a light one.
    enum Colors {
${indent(colors, 2)}

        /// Avatar fallback tints (web \`avatar.N\`), one picked per name.
        static let avatarTints: [Color] = [
${indent(avatarTints, 3)}
        ]
    }

    /// 4pt grid (web \`spacing.N\`). Half steps are for inside controls only.
    enum Spacing {
${indent(spacing, 2)}
    }

    enum Radius {
${indent(pxDims("radii"), 2)}
    }

    enum BorderWidth {
${indent(pxDims("borderWidths"), 2)}
    }

    /// Control heights (comfortable density; touch is the 44pt target).
    enum ControlHeight {
${indent(Object.entries(controlHeights()).map(([n, v]) => `static let ${n}: CGFloat = ${v}`), 2)}
    }

    enum FontSize {
${indent(pxDims("fontSizes"), 2)}
    }

    /// Web \`textStyles\`.
    enum TextStyles {
${indent(textStyles, 2)}
    }

    /// Durations in seconds.
    enum Duration {
${indent(durations, 2)}
    }

    enum Easing {
${indent(easings, 2)}
    }
}

/// Polar Huddle font roles (web \`fonts\` tokens).
enum HuddleFont: CaseIterable {
${indent(huddle.tokenNames("fonts").map((n) => `case ${swiftName(n)}`), 1)}
}

/// A Polar Huddle text style. \`lineHeight\` is a multiple of \`size\`;
/// \`letterSpacing\` is in em.
struct HuddleTextStyle: Equatable {
    let font: HuddleFont
    let size: CGFloat
    let weight: CGFloat
    let lineHeight: CGFloat
    let letterSpacing: CGFloat
}
`;
}

// The Apple accent is the Polar Huddle action colour in each scheme.
const appleAccent = {
  light: literalRgb(huddleColor("action", "daylight")),
  dark: literalRgb(huddleColor("action", "night")),
};

function androidTokensKt() {
  const colors = ANDROID_COLORS.map((name) => [camel(name), resolveColor(name)]);
  const scheme = (pick) =>
    colors.map(([prop, value]) => `        val ${prop} = Color(${value[pick]})`).join("\n");
  const dims = (names, prefix, unit) =>
    names
      .map((n) => `        val ${camel(n === "2xs" ? "xxs" : n === "2xl" ? "xxl" : n === "3xl" ? "xxxl" : n)} = ${remToDp(`${prefix}-${n}`)}.${unit}`)
      .join("\n");
  return `// GENERATED — do not edit.
// Sources: design/src/preset.ts (Huddle),
//          scripts/design-tokens/legacy-tokens.css (everything else)
// Regenerate: bun scripts/generate-design-tokens.mjs
package social.waddle.android.theme

import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
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
  { path: ACCENT_COLORSET, content: colorsetJson(appleAccent) },
  { path: APPLE_TOKENS, content: appleTokensSwift() },
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
