import { copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";

const require = createRequire(new URL("../../../chat/package.json", import.meta.url));
const packageName = process.argv[2] ? resolve(process.argv[2]) : "emojilib";
const source = JSON.parse(readFileSync(require.resolve(packageName), "utf8"));
const license = join(dirname(require.resolve(`${packageName}/package.json`)), "LICENSE");

// Match chat/src/lib/emoji.ts: the first glyph wins for each keyword,
// and Object.entries enumerates integer keys before the other keywords.
const shortcodes = {};
for (const [glyph, keywords] of Object.entries(source)) {
  for (const name of keywords) {
    if (!shortcodes[name]) shortcodes[name] = glyph;
  }
}
const rows = Object.entries(shortcodes).map(([name, glyph]) => ({ name, glyph }));

const resources = new URL("../WaddleKit/Sources/WaddleKit/Compose/Suggestions/Resources/", import.meta.url);
const table = "[\n" + rows.map((row) => JSON.stringify(row)).join(",\n") + "\n]\n";
writeFileSync(new URL("EmojiShortcodes.json", resources), table);
copyFileSync(license, new URL("EmojiShortcodes-LICENSE.txt", resources));
