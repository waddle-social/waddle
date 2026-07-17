import { defineConfig } from "@playwright/test";

const port = Number.parseInt(
  process.env.WADDLE_PLAYWRIGHT_PORT ?? "43189",
  10,
);
if (!Number.isSafeInteger(port) || port < 1 || port > 65_535) {
  throw new RangeError("WADDLE_PLAYWRIGHT_PORT must be a valid TCP port");
}

export default defineConfig({
  testDir: ".",
  testMatch: "**/*.browser.ts",
  fullyParallel: false,
  workers: 1,
  reporter: "line",
  outputDir: `/tmp/waddle-playwright-p0-1-${process.pid}`,
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    channel: "chrome",
    headless: true,
    screenshot: "off",
    trace: "off",
    video: "off",
  },
  webServer: {
    command: `bunx astro dev --config tests/browser/astro.config.mjs --host 127.0.0.1 --port ${port}`,
    url: `http://127.0.0.1:${port}`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
