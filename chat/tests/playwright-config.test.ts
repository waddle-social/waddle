import { describe, expect, test } from "bun:test";
import { fileURLToPath } from "node:url";
import playwrightConfig from "./browser/playwright.config";

describe("durability browser test contract", () => {
  test("resolves the chat fixture and both real Chrome specifications", () => {
    const chatRoot = fileURLToPath(new URL("..", import.meta.url));
    const browserRoot = fileURLToPath(new URL("browser", import.meta.url));
    const browserSpecs = [...new Bun.Glob("*.browser.ts").scanSync({
      cwd: browserRoot,
    })].sort();
    const webServer = Array.isArray(playwrightConfig.webServer)
      ? playwrightConfig.webServer[0]
      : playwrightConfig.webServer;

    expect(webServer).toEqual(expect.objectContaining({
      command:
        "bunx astro dev --config tests/browser/astro.config.mjs --host 127.0.0.1 --port 43189",
      cwd: chatRoot,
      url: "http://127.0.0.1:43189/durable",
    }));
    expect(playwrightConfig.use).toMatchObject({
      channel: "chrome",
      headless: true,
    });
    expect(playwrightConfig.testDir).toBe(".");
    expect(playwrightConfig.testMatch).toBe("**/*.browser.ts");
    expect(browserSpecs).toEqual([
      "durable-runtime.browser.ts",
      "xmpp-provider.browser.ts",
    ]);
  });
});
