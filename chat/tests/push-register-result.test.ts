// Pins the XEP-0050 `register-device` result guard extracted from
// `client.ts`. The invariant under test: the chat persists a device
// registration ONLY when both `node` and `deviceId` are present and
// non-empty. A missing/empty `deviceId` would force the per-device
// `disable-device` opt-out into disable-everywhere semantics that take
// down sibling devices on the same XEP-0357 node.

import { describe, expect, test } from "bun:test";
import {
  parseRegisterDeviceResult,
  parseRegisterPushDeviceRejection,
  retryRegisterPushDeviceAfterSessionExpired,
} from "../src/lib/xmpp/push-register-result";

describe("parseRegisterDeviceResult", () => {
  test("accepts a well-formed (node, deviceId) pair", () => {
    expect(parseRegisterDeviceResult({ node: "node-1", deviceId: "device-1" })).toEqual({
      node: "node-1",
      deviceId: "device-1",
    });
  });

  test("rejects an empty node", () => {
    expect(parseRegisterDeviceResult({ node: "", deviceId: "device-1" })).toBeNull();
  });

  test("rejects an empty deviceId (would force disable-everywhere)", () => {
    expect(parseRegisterDeviceResult({ node: "node-1", deviceId: "" })).toBeNull();
  });

  test("rejects a missing deviceId", () => {
    expect(parseRegisterDeviceResult({ node: "node-1" })).toBeNull();
  });

  test("rejects a missing node", () => {
    expect(parseRegisterDeviceResult({ deviceId: "device-1" })).toBeNull();
  });

  test("rejects non-string fields", () => {
    expect(parseRegisterDeviceResult({ node: 1, deviceId: 2 })).toBeNull();
  });

  test("rejects null, undefined, and non-objects", () => {
    expect(parseRegisterDeviceResult(null)).toBeNull();
    expect(parseRegisterDeviceResult(undefined)).toBeNull();
    expect(parseRegisterDeviceResult("nope")).toBeNull();
  });

  test("ignores extra fields and returns only node + deviceId", () => {
    expect(
      parseRegisterDeviceResult({ node: "node-1", deviceId: "device-1", leaked: "secret" }),
    ).toEqual({ node: "node-1", deviceId: "device-1" });
  });
});

describe("registerPushDevice session-expired retry", () => {
  test("parses structured wasm rejection objects", () => {
    expect(
      parseRegisterPushDeviceRejection({
        code: "session-expired",
        message: "XEP-0050 command session expired",
      }),
    ).toEqual({
      code: "session-expired",
      message: "XEP-0050 command session expired",
    });
    expect(parseRegisterPushDeviceRejection("session expired")).toBeNull();
  });

  test("retries exactly once after session-expired", async () => {
    let calls = 0;
    const result = await retryRegisterPushDeviceAfterSessionExpired(async () => {
      calls += 1;
      if (calls === 1) {
        throw { code: "session-expired", message: "expired" };
      }
      return { node: "node-1", deviceId: "device-1" };
    });

    expect(result).toEqual({ node: "node-1", deviceId: "device-1" });
    expect(calls).toBe(2);
  });

  test("does not retry non-session failures", async () => {
    let calls = 0;
    let thrown: unknown = null;
    try {
      await retryRegisterPushDeviceAfterSessionExpired(async () => {
        calls += 1;
        throw { code: "stanza-error", message: "forbidden" };
      });
    } catch (error) {
      thrown = error;
    }

    // Non-session failures PROPAGATE (they are not retryable and the
    // caller's error handling must see them unchanged — swallowing
    // into null would wrongly clear persisted push ids over a blip).
    expect(thrown).toEqual({ code: "stanza-error", message: "forbidden" });
    expect(calls).toBe(1);
  });

  test("a transient pre-WASM exception propagates untouched", async () => {
    let thrown: unknown = null;
    try {
      await retryRegisterPushDeviceAfterSessionExpired(async () => {
        throw new Error("xmpp not connected");
      });
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(Error);
  });

  test("second session-expired is terminal: null after the single retry", async () => {
    let calls = 0;
    const result = await retryRegisterPushDeviceAfterSessionExpired(async () => {
      calls += 1;
      throw { code: "session-expired", message: "expired" };
    });

    // Terminal registration failure → the caller's null-path clears
    // the persisted ids (same as any terminal register failure).
    expect(result).toBeNull();
    expect(calls).toBe(2);
  });

  test("a non-session failure on the retry attempt propagates", async () => {
    let calls = 0;
    let thrown: unknown = null;
    try {
      await retryRegisterPushDeviceAfterSessionExpired(async () => {
        calls += 1;
        if (calls === 1) throw { code: "session-expired", message: "expired" };
        throw new Error("network dropped mid-retry");
      });
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(Error);
    expect(calls).toBe(2);
  });
});
