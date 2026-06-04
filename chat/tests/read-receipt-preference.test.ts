import { describe, expect, test } from "bun:test";
import {
  readStoredReadReceiptPreference,
  writeStoredReadReceiptPreference,
} from "../src/lib/read-receipt-preference";

describe("read receipt preference storage", () => {
  test("falls back to send when storage reads throw", () => {
    const throwingStorage = {
      getItem() {
        throw new Error("storage disabled");
      },
    };

    expect(readStoredReadReceiptPreference(throwingStorage)).toBe("send");
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

    expect(() => writeStoredReadReceiptPreference("send", throwingStorage)).not.toThrow();
    expect(() => writeStoredReadReceiptPreference("suppress", throwingStorage)).not.toThrow();
  });
});
