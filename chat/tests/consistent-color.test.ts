import { describe, expect, test } from "bun:test";
import { consistentColor, setConsistentColorBackend } from "../src/lib/chat-ui";
import {
  readStoredConsistentColorPreference,
  writeStoredConsistentColorPreference,
} from "../src/lib/consistent-color-preference";

function memoryStorage() {
  const data = new Map<string, string>();
  return {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    removeItem: (k: string) => void data.delete(k),
  };
}

describe("consistentColor", () => {
  test("is null (static colour) until the wasm backend is installed", () => {
    expect(consistentColor("romeo")).toBeNull();
  });

  test("passes input, saturation and lightness to the installed backend", () => {
    const calls: Array<[string, number, number]> = [];
    setConsistentColorBackend((input, s, l) => {
      calls.push([input, s, l]);
      return "#dd00af";
    });
    expect(consistentColor("Romeo", 100, 50)).toBe("#dd00af");
    expect(consistentColor("Romeo")).toBe("#dd00af");
    expect(calls).toEqual([
      ["Romeo", 100, 50],
      ["Romeo", 100, 45],
    ]);
  });

  test("ignores a non-function backend from a stale wasm bundle", () => {
    const before = consistentColor("x");
    setConsistentColorBackend(undefined as never);
    expect(consistentColor("x")).toBe(before);
  });
});

describe("consistent colour preference", () => {
  test("defaults to on and round-trips off", () => {
    const store = memoryStorage();
    expect(readStoredConsistentColorPreference(store)).toBe("on");
    writeStoredConsistentColorPreference("off", store);
    expect(readStoredConsistentColorPreference(store)).toBe("off");
    writeStoredConsistentColorPreference("on", store);
    expect(readStoredConsistentColorPreference(store)).toBe("on");
    expect(store.getItem("waddle:consistent-color")).toBeNull();
  });

  test("survives unavailable storage", () => {
    const broken = {
      getItem: () => {
        throw new Error("blocked");
      },
    };
    expect(readStoredConsistentColorPreference(broken)).toBe("on");
    expect(readStoredConsistentColorPreference(null)).toBe("on");
  });
});
