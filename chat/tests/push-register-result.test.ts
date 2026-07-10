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
    const result = await retryRegisterPushDeviceAfterSessionExpired(async () => {
      calls += 1;
      throw { code: "stanza-error", message: "forbidden" };
    });

    expect(result).toBeNull();
    expect(calls).toBe(1);
  });

  test("gives up after one failed session-expired retry", async () => {
    let calls = 0;
    const result = await retryRegisterPushDeviceAfterSessionExpired(async () => {
      calls += 1;
      throw { code: "session-expired", message: "expired" };
    });

    expect(result).toBeNull();
    expect(calls).toBe(2);
  });
});
