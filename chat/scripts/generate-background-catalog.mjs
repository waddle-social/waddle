/**
 * Generates the bundled virtual-background catalog images (#1024) as small,
 * dependency-free gradient PNGs committed under `public/backgrounds/`. These are
 * the self-hosted replacement backgrounds the call-settings "Background" picker
 * offers; they are authored here (no third-party assets, no licensing) and kept
 * deliberately simple so they composite cleanly behind a segmented foreground.
 *
 * Re-run with `bun run scripts/generate-background-catalog.mjs` to regenerate.
 */
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const WIDTH = 1280;
const HEIGHT = 720;

/** id → two-stop vertical gradient (top → bottom), each [r, g, b]. */
const CATALOG = {
  mountain: [
    [70, 110, 160],
    [180, 205, 220],
  ],
  office: [
    [98, 92, 86],
    [206, 198, 188],
  ],
  abstract: [
    [104, 58, 168],
    [214, 96, 154],
  ],
};

function crc32(bytes) {
  let crc = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) {
    crc ^= bytes[i];
    for (let bit = 0; bit < 8; bit++) {
      crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const typeBytes = Buffer.from(type, "ascii");
  const body = Buffer.concat([typeBytes, data]);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([length, body, crc]);
}

/** Encode an RGB gradient as a PNG buffer. */
function gradientPng([top, bottom]) {
  // Raw scanlines: a filter byte (0 = none) then RGB triples per pixel.
  const raw = Buffer.alloc(HEIGHT * (1 + WIDTH * 3));
  let offset = 0;
  for (let y = 0; y < HEIGHT; y++) {
    raw[offset++] = 0;
    const t = y / (HEIGHT - 1);
    const r = Math.round(top[0] + (bottom[0] - top[0]) * t);
    const g = Math.round(top[1] + (bottom[1] - top[1]) * t);
    const b = Math.round(top[2] + (bottom[2] - top[2]) * t);
    for (let x = 0; x < WIDTH; x++) {
      raw[offset++] = r;
      raw[offset++] = g;
      raw[offset++] = b;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(WIDTH, 0);
  ihdr.writeUInt32BE(HEIGHT, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // colour type: truecolour (RGB)
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  return Buffer.concat([
    signature,
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const outDir = resolve(import.meta.dirname, "../public/backgrounds");
mkdirSync(outDir, { recursive: true });
for (const [id, stops] of Object.entries(CATALOG)) {
  const file = resolve(outDir, `${id}.png`);
  writeFileSync(file, gradientPng(stops));
  console.log(`[bg-catalog] wrote ${file}`);
}
