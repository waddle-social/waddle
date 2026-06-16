import { describe, expect, test } from "bun:test";
import {
  currentScreenCaptureEnv,
  hasSafari17CaptureBug,
} from "../src/lib/calls/video-codec/screen-capture-env";

// Representative user-agent strings.
const MACOS_SAFARI_17 =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";
const MACOS_SAFARI_18 =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15";
const MACOS_CHROME =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const CHROME_IOS =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/124.0 Mobile/15E148 Safari/604.1";

describe("hasSafari17CaptureBug — the WebKit getDisplayMedia resolution bug", () => {
  test("macOS Safari 17.x is affected", () => {
    expect(hasSafari17CaptureBug(MACOS_SAFARI_17)).toBe(true);
  });

  test("macOS Safari 18 is not affected (bug is 17.x-specific)", () => {
    expect(hasSafari17CaptureBug(MACOS_SAFARI_18)).toBe(false);
  });

  test("Chromium-based browsers are never affected, even on Apple hardware", () => {
    expect(hasSafari17CaptureBug(MACOS_CHROME)).toBe(false);
    expect(hasSafari17CaptureBug(CHROME_IOS)).toBe(false);
  });
});

describe("currentScreenCaptureEnv — read the real environment", () => {
  test("a Safari-17 user-agent cannot be given an explicit capture resolution", () => {
    expect(currentScreenCaptureEnv(MACOS_SAFARI_17).canConstrainResolution).toBe(false);
  });

  test("everywhere else, an explicit resolution is safe", () => {
    expect(currentScreenCaptureEnv(MACOS_CHROME).canConstrainResolution).toBe(true);
  });
});
