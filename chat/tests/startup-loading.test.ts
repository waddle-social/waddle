import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import {
  STARTUP_SHELL_MARKERS,
  selectStartupShellChunk,
} from "../scripts/check-startup-build";

describe("startup loading fallback", () => {
  test("source includes the static shell before AppShell hydrates", () => {
    const html = readFileSync(
      new URL("../src/layouts/AppLayout.astro", import.meta.url),
      "utf8",
    );

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

  test("selects the single complete emitted shell candidate", () => {
    expect(selectStartupShellChunk([
      { name: "z.mjs", contents: "unrelated" },
      { name: "shell.mjs", contents: STARTUP_SHELL_MARKERS.join(" ") },
    ]).name).toBe("shell.mjs");
  });

  test("reports sorted chunk names when no shell candidate exists", () => {
    expect(() => selectStartupShellChunk([
      { name: "z.mjs", contents: "Loading Waddle" },
      { name: "a.mjs", contents: "unrelated" },
    ])).toThrow(
      "no startup-shell candidate found; scanned chunks: a.mjs, z.mjs",
    );
  });

  test("rejects ambiguous shell candidates in sorted order", () => {
    const marker = STARTUP_SHELL_MARKERS[0];
    expect(() => selectStartupShellChunk([
      { name: "z-shell.mjs", contents: marker },
      { name: "a-shell.mjs", contents: marker },
    ])).toThrow(
      "ambiguous startup-shell candidates: a-shell.mjs, z-shell.mjs",
    );
  });

  test("names the sole candidate and every missing contract marker", () => {
    expect(() => selectStartupShellChunk([
      {
        name: "partial-shell.mjs",
        contents: STARTUP_SHELL_MARKERS[0],
      },
    ])).toThrow(
      'startup-shell candidate partial-shell.mjs is missing markers: "Loading Waddle", "Checking session."',
    );
  });
});
