import { describe, expect, test } from "bun:test";
import {
  callUiModeAfterFullscreenExit,
  callUiModeAfterSurfaceEscape,
  nextCallUiMode,
  resetCallUiModeAfterCallEnd,
  shouldExitNativeFullscreenForModeChange,
} from "../src/lib/calls/ui-mode";

describe("call UI mode transitions", () => {
  test("cycles split to expanded to immersive", () => {
    expect(nextCallUiMode("split")).toBe("expanded");
    expect(nextCallUiMode("expanded")).toBe("immersive");
    expect(nextCallUiMode("immersive")).toBe("expanded");
  });

  test("Esc from immersive returns to expanded", () => {
    expect(callUiModeAfterSurfaceEscape("immersive")).toBe("expanded");
    expect(callUiModeAfterSurfaceEscape("expanded")).toBe("split");
  });

  test("native fullscreen exit keeps non-immersive modes stable", () => {
    expect(callUiModeAfterFullscreenExit("immersive")).toBe("expanded");
    expect(callUiModeAfterFullscreenExit("expanded")).toBe("expanded");
    expect(callUiModeAfterFullscreenExit("split")).toBe("split");
  });

  test("leaving immersive exits native fullscreen only when it is layered on", () => {
    expect(shouldExitNativeFullscreenForModeChange("immersive", "expanded", true)).toBe(true);
    expect(shouldExitNativeFullscreenForModeChange("immersive", "expanded", false)).toBe(false);
    expect(shouldExitNativeFullscreenForModeChange("expanded", "immersive", true)).toBe(false);
    expect(shouldExitNativeFullscreenForModeChange("expanded", "split", true)).toBe(false);
  });

  test("call end resets the next call to split", () => {
    expect(resetCallUiModeAfterCallEnd()).toBe("split");
  });
});
