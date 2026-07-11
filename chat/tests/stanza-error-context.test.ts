/**
 * Unit tests for the shared stanza-error context extractor
 * (`src/lib/xmpp/stanza-error-context.ts`): the wasm bridge's structured
 * `Error` rejections, the legacy object shapes, errorType validation, and
 * the telemetry length cap on server-provided error text.
 */
import { describe, expect, test } from "bun:test";
import { stanzaErrorContext } from "../src/lib/xmpp/stanza-error-context";

describe("stanzaErrorContext", () => {
  test("reads condition, errorType, and text off a structured Error rejection", () => {
    const rejection = Object.assign(
      new Error("server returned a stanza error: cancel: item-not-found"),
      { condition: "item-not-found", errorType: "cancel", text: "no such room" },
    );
    expect(stanzaErrorContext(rejection)).toEqual({
      condition: "item-not-found",
      errorType: "cancel",
      errorText: "no such room",
    });
  });

  test("supports the legacy nested { error: { condition } } shape", () => {
    expect(stanzaErrorContext({ error: { condition: "service-unavailable" } })).toEqual({
      condition: "service-unavailable",
    });
  });

  test("drops an errorType outside the RFC 6120 §8.3.2 value set", () => {
    expect(
      stanzaErrorContext({ condition: "forbidden", errorType: "bogus" }),
    ).toEqual({ condition: "forbidden" });
  });

  test("truncates oversized server error text for the beacon", () => {
    const context = stanzaErrorContext({
      condition: "resource-constraint",
      text: "x".repeat(10_000),
    });
    expect(context.errorText).toHaveLength(256);
  });

  test("yields an empty context for strings and unshaped rejections", () => {
    expect(stanzaErrorContext("client is disconnected")).toEqual({});
    expect(stanzaErrorContext(new Error("boom"))).toEqual({});
    expect(stanzaErrorContext(null)).toEqual({});
  });
});
