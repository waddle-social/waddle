import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("startup loading fallback", () => {
  test("renders a static shell before ChatApp hydrates", () => {
    const layout = readFileSync(new URL("../src/layouts/AppLayout.astro", import.meta.url), "utf8");

    expect(layout).toContain('<ChatApp client:only="vue" giphyApiKey={giphyApiKey}>');
    expect(layout).toContain('slot="fallback"');
    expect(layout).toContain("chat-app-shell chat-startup-shell");
    expect(layout).toContain("Checking session.");
  });

  test("keeps fallback dimensions stable without Vue", () => {
    const styles = readFileSync(new URL("../src/styles/global.css", import.meta.url), "utf8");

    expect(styles).toContain(".chat-app-shell");
    expect(styles).toContain("height: 100dvh;");
    expect(styles).toContain(".chat-startup-main");
    expect(styles).toContain("flex: 1 1 0%;");
  });
});
