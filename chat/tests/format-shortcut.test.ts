import { describe, expect, test } from "bun:test";
import { formatModShortcut, isMacPlatform, type NavigatorLike } from "../src/lib/composer/format-shortcut";

const macUserAgentData: NavigatorLike = { userAgentData: { platform: "macOS" } };
const macPlatform: NavigatorLike = { platform: "MacIntel" };
const iPad: NavigatorLike = { userAgent: "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X)" };
const linuxFirefox: NavigatorLike = {
  platform: "Linux x86_64",
  userAgent: "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0",
};
const windows: NavigatorLike = { platform: "Win32" };

describe("isMacPlatform", () => {
  test("trusts modern userAgentData.platform", () => {
    expect(isMacPlatform(macUserAgentData)).toBe(true);
  });

  test("falls back to legacy navigator.platform", () => {
    expect(isMacPlatform(macPlatform)).toBe(true);
  });

  test("detects iOS as Mac for shortcut purposes", () => {
    expect(isMacPlatform(iPad)).toBe(true);
  });

  test("returns false for Linux", () => {
    expect(isMacPlatform(linuxFirefox)).toBe(false);
  });

  test("returns false for Windows", () => {
    expect(isMacPlatform(windows)).toBe(false);
  });

  test("returns false when navigator is missing", () => {
    expect(isMacPlatform(undefined)).toBe(false);
  });
});

describe("formatModShortcut", () => {
  test("renders ⌘ glyph on Mac without separators", () => {
    expect(formatModShortcut("Mod-B", macPlatform)).toBe("⌘B");
  });

  test("renders Ctrl+ prefix off Mac with + separators", () => {
    expect(formatModShortcut("Mod-B", windows)).toBe("Ctrl+B");
  });

  test("composes multiple modifiers on Mac", () => {
    expect(formatModShortcut("Mod-Shift-X", macPlatform)).toBe("⌘⇧X");
  });

  test("composes multiple modifiers off Mac", () => {
    expect(formatModShortcut("Mod-Shift-X", linuxFirefox)).toBe("Ctrl+Shift+X");
  });

  test("handles Alt on both platforms", () => {
    expect(formatModShortcut("Mod-Alt-C", macPlatform)).toBe("⌘⌥C");
    expect(formatModShortcut("Mod-Alt-C", windows)).toBe("Ctrl+Alt+C");
  });
});
