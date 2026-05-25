// Coverage for the cleared-localStorage rotation guard.
//
// `ensureBrowserSubscriptionWithCurrentKey` (chat/src/shell/notifications.ts)
// triggers re-subscription whenever the existing PushSubscription's
// `applicationServerKey` bytes don't match the freshly-advertised
// public key — i.e. when localStorage was wiped (private mode,
// "Clear site data", inherited SW from a different account) and we
// can't trust the surviving subscription is bound to the current key.
// This test pins the byte-level comparison semantics.

import { describe, expect, test } from "bun:test";
import { subscriptionApplicationKeyMatches } from "../src/shell/notifications";

/** 65-byte uncompressed SEC1 P-256 point — first byte 0x04, rest fill. */
function samplePointBytes(seed = 0xab): Uint8Array {
  const bytes = new Uint8Array(65);
  bytes[0] = 0x04;
  for (let i = 1; i < bytes.length; i++) {
    bytes[i] = (seed + i) & 0xff;
  }
  return bytes;
}

function urlBase64NoPad(bytes: Uint8Array): string {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

describe("subscriptionApplicationKeyMatches", () => {
  test("returns true when both sides hold the same 65-byte SEC1 point (ArrayBuffer)", () => {
    const bytes = samplePointBytes();
    const advertised = urlBase64NoPad(bytes);
    expect(
      subscriptionApplicationKeyMatches({ options: { applicationServerKey: bytes.buffer } }, advertised),
    ).toBe(true);
  });

  test("returns true when applicationServerKey is a Uint8Array view", () => {
    const bytes = samplePointBytes();
    const advertised = urlBase64NoPad(bytes);
    // Wrap bytes inside a larger buffer to confirm byteOffset is honored.
    const wrapper = new Uint8Array(128);
    wrapper.set(bytes, 32);
    const view = new Uint8Array(wrapper.buffer, 32, 65);
    expect(
      subscriptionApplicationKeyMatches({ options: { applicationServerKey: view } }, advertised),
    ).toBe(true);
  });

  test("returns false when the bytes differ (rotation case)", () => {
    const oldBytes = samplePointBytes(0xab);
    const newBytes = samplePointBytes(0xcd);
    const advertised = urlBase64NoPad(newBytes);
    expect(
      subscriptionApplicationKeyMatches({ options: { applicationServerKey: oldBytes.buffer } }, advertised),
    ).toBe(false);
  });

  test("returns false when the lengths differ", () => {
    const bytes = samplePointBytes();
    const truncated = bytes.slice(0, 64);
    const advertised = urlBase64NoPad(bytes);
    expect(
      subscriptionApplicationKeyMatches({ options: { applicationServerKey: truncated.buffer } }, advertised),
    ).toBe(false);
  });

  test("returns true when subscription has no applicationServerKey (no comparison applicable)", () => {
    expect(subscriptionApplicationKeyMatches({ options: { applicationServerKey: null } }, "any")).toBe(
      true,
    );
    expect(subscriptionApplicationKeyMatches({ options: {} }, "any")).toBe(true);
  });

  test("returns false when the advertised value is unparseable base64url", () => {
    const bytes = samplePointBytes();
    expect(
      subscriptionApplicationKeyMatches(
        { options: { applicationServerKey: bytes.buffer } },
        "!!! not base64 !!!",
      ),
    ).toBe(false);
  });
});
