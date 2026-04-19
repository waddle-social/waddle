import { mkdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import sharp from "sharp";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDir, "..");
const svgPath = resolve(projectRoot, "public", "icon.svg");
const outDir = resolve(projectRoot, "public", "icons");

mkdirSync(outDir, { recursive: true });

const svg = readFileSync(svgPath);

const sizes = [
  { name: "icon-192x192.png", size: 192 },
  { name: "icon-512x512.png", size: 512 },
  { name: "apple-touch-icon-180x180.png", size: 180 },
];

for (const { name, size } of sizes) {
  await sharp(svg).resize(size, size).png().toFile(resolve(outDir, name));
  console.log(`Generated ${name}`);
}
