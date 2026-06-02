import { readdirSync, rmSync, statSync } from "node:fs";
import { resolve } from "node:path";

const sourceMapRoot = resolve("dist", "client");
let removed = 0;

try {
  if (!statSync(sourceMapRoot).isDirectory()) {
    process.exit(0);
  }
} catch {
  process.exit(0);
}

for (const sourceMap of findSourceMaps(sourceMapRoot)) {
  rmSync(sourceMap, { force: true });
  removed += 1;
}

if (removed > 0) {
  console.log(`[faro] removed ${removed} source maps from dist/client`);
}

function findSourceMaps(root) {
  const files = [];
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) {
        visit(path);
      } else if (entry.isFile() && entry.name.endsWith(".map")) {
        files.push(path);
      }
    }
  };
  visit(root);
  return files;
}
