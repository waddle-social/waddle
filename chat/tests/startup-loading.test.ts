import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";

function builtAppLayoutChunk(): string {
  const chunksDir = new URL("../dist/server/chunks/", import.meta.url);
  const chunk = readdirSync(chunksDir).find((name) => name.startsWith("AppLayout_") && name.endsWith(".mjs"));
  if (!chunk) throw new Error("built AppLayout chunk not found; run `bun run build` before this test");
  return readFileSync(new URL(chunk, chunksDir), "utf8");
}

describe("startup loading fallback", () => {
  test("build output includes the static shell before AppShell hydrates", () => {
    const html = builtAppLayoutChunk();

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
