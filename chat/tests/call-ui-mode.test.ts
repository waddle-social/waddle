import { afterEach, describe, expect, test } from "bun:test";
import {
  callUiModeAfterFullscreenExit,
  callUiModeAfterSurfaceEscape,
  nextCallUiMode,
  resetCallUiModeAfterCallEnd,
  shouldExitNativeFullscreenForModeChange,
} from "../src/lib/calls/ui-mode";

const originalWindow = globalThis.window;

function memoryStorage(seed: Record<string, string> = {}): Storage {
  const values = new Map(Object.entries(seed));
  return {
    get length() {
      return values.size;
    },
    clear() {
      values.clear();
    },
    getItem(key: string) {
      return values.get(key) ?? null;
    },
    key(index: number) {
      return [...values.keys()][index] ?? null;
    },
    removeItem(key: string) {
      values.delete(key);
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
  };
}

afterEach(() => {
  if (originalWindow === undefined) {
    Reflect.deleteProperty(globalThis, "window");
    return;
  }
  globalThis.window = originalWindow;
});

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

  test("fresh loads restore split or expanded, but not immersive", async () => {
    const loadMode = async (storedMode: string) => {
      globalThis.window = {
        localStorage: memoryStorage({ "waddle:call-ui-mode": storedMode }),
      } as Window & typeof globalThis;
      const module = await import(`../src/lib/calls/ui-mode?stored=${storedMode}`);
      return module.$callUiMode.get();
    };

    expect(await loadMode("split")).toBe("split");
    expect(await loadMode("expanded")).toBe("expanded");
    expect(await loadMode("immersive")).toBe("split");
  });
});
