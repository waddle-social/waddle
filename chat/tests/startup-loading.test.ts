import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";

// Find whichever Vite-emitted chunk contains the static fallback shell —
// the chunk name depends on Rollup's choice of entrypoint and isn't
// stable (it used to be `AppLayout_*.mjs`; after the per-route island
// split it lives in a router/registry-anchored chunk). The shell HTML is
// the contract we care about, not the filename.
function findShellChunk(): string {
  const chunksDir = new URL("../dist/server/chunks/", import.meta.url);
  for (const name of readdirSync(chunksDir)) {
    if (!name.endsWith(".mjs")) continue;
    const contents = readFileSync(new URL(name, chunksDir), "utf8");
    if (contents.includes("chat-app-shell chat-startup-shell")) {
      return contents;
    }
  }
  throw new Error("built static-shell chunk not found; run `bun run build` before this test");
}

describe("startup loading fallback", () => {
  test("build output includes the static shell before AppShell hydrates", () => {
    const html = findShellChunk();

    expect(html).toContain("chat-app-shell chat-startup-shell");
    expect(html).toContain("Loading Waddle");
    expect(html).toContain("Checking session.");
  });

  test("keeps fallback dimensions stable without Vue", () => {
    const styles = readFileSync(
      new URL("../src/styles/global/shell.css", import.meta.url),
      "utf8",
    );

    expect(styles).toContain(".chat-app-shell");
    expect(styles).toContain("height: 100dvh;");
    expect(styles).toContain(".chat-startup-main");
    expect(styles).toContain("flex: 1 1 0%;");
  });
});
