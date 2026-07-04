#!/usr/bin/env bun
// Design-token generator.
//
// Source of truth: chat/src/styles/global/tokens.css (human-edited).
// Generated outputs (never edit by hand):
//   - apps/apple/Waddle/Assets.xcassets/AccentColor.colorset/Contents.json
//   - website/src/styles/global/brand-palette.css
//
// Usage:
//   bun scripts/generate-design-tokens.mjs           # write outputs
//   bun scripts/generate-design-tokens.mjs --check   # exit 1 if outputs are stale
//
// Scope is deliberately narrow: only the brand accent (chat `--primary`)
// is shared across all three surfaces today. Target-specific palettes
// (chat's full Aether token sheet, the website hero teals, WaddleTheme's
// presence colors) stay owned by their surfaces — mapping them here
// without a design decision would guess at semantics.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

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

const outputs = [
  { path: ACCENT_COLORSET, content: colorsetJson(primary) },
  { path: WEBSITE_PALETTE, content: websitePaletteCss(primary) },
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
