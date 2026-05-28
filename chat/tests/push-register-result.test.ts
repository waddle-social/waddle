// Pins the XEP-0050 `register-device` result guard extracted from
// `client.ts`. The invariant under test: the chat persists a device
// registration ONLY when both `node` and `deviceId` are present and
// non-empty. A missing/empty `deviceId` would force the per-device
// `disable-device` opt-out into disable-everywhere semantics that take
// down sibling devices on the same XEP-0357 node.

import { describe, expect, test } from "bun:test";
import { parseRegisterDeviceResult } from "../src/lib/xmpp/push-register-result";

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
