import { afterEach, describe, expect, test } from "bun:test";
import {
  CALL_CORRELATION_ID_HEX_LEN,
  UNKNOWN_CALL_CORRELATION_ID,
  adoptCallCorrelationId,
  callCorrelationId,
  clearCallCorrelationId,
  deriveCallCorrelationId,
} from "../src/lib/calls/call-correlation";

afterEach(() => {
  clearCallCorrelationId();
});

describe("deriveCallCorrelationId", () => {
  /**
   * Cross-implementation pin. The Rust side
   * (`server/crates/waddle-sfu/src/correlation.rs`) asserts the same
   * vector; if either changes, client and server telemetry stop joining.
   */
  test("matches the pinned server digest for the same room name", async () => {
    await expect(deriveCallCorrelationId("general@muc.example.com")).resolves.toBe(
      "ba2798ebd1a58db8",
    );
  });

  test("is stable and bounded lowercase hex", async () => {
    const id = await deriveCallCorrelationId("alice@example.com::dm-1128");
    expect(id).toHaveLength(CALL_CORRELATION_ID_HEX_LEN);
    expect(id).toMatch(/^[0-9a-f]+$/);
    await expect(deriveCallCorrelationId("alice@example.com::dm-1128")).resolves.toBe(id);
  });

  test("leaks no substring of the room name it was derived from", async () => {
    const id = await deriveCallCorrelationId("alice@example.com::dm-1128");
    expect(id).not.toContain("alice");
    expect(id).not.toContain("example");
    expect(id).not.toContain("dm-1128");
  });

  test("different rooms get different ids", async () => {
    const a = await deriveCallCorrelationId("general@muc.example.com");
    const b = await deriveCallCorrelationId("random@muc.example.com");
    expect(a).not.toBe(b);
  });

  test("an empty room name resolves to the unknown sentinel, never throws", async () => {
    await expect(deriveCallCorrelationId("")).resolves.toBe(UNKNOWN_CALL_CORRELATION_ID);
  });
});

describe("current call correlation id", () => {
  test("defaults to unknown before any call is adopted", () => {
    expect(callCorrelationId()).toBe(UNKNOWN_CALL_CORRELATION_ID);
  });

  test("adopting a room makes its id the one telemetry stamps", async () => {
    const adopted = await adoptCallCorrelationId("general@muc.example.com");
    expect(adopted).toBe("ba2798ebd1a58db8");
    expect(callCorrelationId()).toBe(adopted);
  });

  test("clearing prevents a post-call event being attributed to the last call", async () => {
    await adoptCallCorrelationId("general@muc.example.com");
    clearCallCorrelationId();
    expect(callCorrelationId()).toBe(UNKNOWN_CALL_CORRELATION_ID);
  });
});
