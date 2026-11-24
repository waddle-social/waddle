import { describe, expect, test } from "bun:test";
import {
  readStoredChatStatePreference,
  writeStoredChatStatePreference,
} from "../src/lib/chat-state-preference";

describe("chat state preference storage", () => {
  test("falls back to send when storage reads throw", () => {
    const throwingStorage = {
      getItem() {
        throw new Error("storage disabled");
      },
    };

    expect(readStoredChatStatePreference(throwingStorage)).toBe("send");
  });

  test("write failures do not throw", () => {
    const throwingStorage = {
      removeItem() {
        throw new Error("storage disabled");
      },
      setItem() {
        throw new Error("storage disabled");
      },
    };

    expect(() => writeStoredChatStatePreference("send", throwingStorage)).not.toThrow();
    expect(() => writeStoredChatStatePreference("suppress", throwingStorage)).not.toThrow();
  });
});

describe("chat state preference round trip", () => {
  test("defaults to send and persists suppress", () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    };
    expect(readStoredChatStatePreference(storage)).toBe("send");
    writeStoredChatStatePreference("suppress", storage);
    expect(readStoredChatStatePreference(storage)).toBe("suppress");
    writeStoredChatStatePreference("send", storage);
    expect(readStoredChatStatePreference(storage)).toBe("send");
  });
});
