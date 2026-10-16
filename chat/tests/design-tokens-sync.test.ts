import { describe, expect, test } from "bun:test";
import { join } from "node:path";

// The generated token outputs (apple AccentColor asset, website
// brand-palette.css, Android WaddleTokens.kt) must stay in sync with the
// sources of truth, chat/src/styles/global/tokens.css and
// chat/src/styles/huddle-tokens.ts. The generator's --check mode exits
// non-zero when any output is stale.
describe("design tokens", () => {
  test("generated outputs are in sync with their sources", async () => {
    const repoRoot = join(import.meta.dir, "..", "..");
    const proc = Bun.spawn(
      ["bun", join(repoRoot, "scripts", "generate-design-tokens.mjs"), "--check"],
      { stdout: "pipe", stderr: "pipe" },
    );
    const exitCode = await proc.exited;
    const stderr = await new Response(proc.stderr).text();
    expect(exitCode, stderr).toBe(0);
  });
});
