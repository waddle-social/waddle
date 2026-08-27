/**
 * Unit tests for the shared stanza-error context extractor
 * (`src/lib/xmpp/stanza-error-context.ts`): the wasm bridge's structured
 * `Error` rejections, the legacy object shapes, and errorType validation.
 */
import { describe, expect, test } from "bun:test";
import { stanzaErrorContext } from "../src/lib/xmpp/stanza-error-context";

describe("stanzaErrorContext", () => {
  test("reads condition and errorType off a structured Error rejection", () => {
    const rejection = Object.assign(
      new Error("server returned a stanza error: cancel: item-not-found"),
      { condition: "item-not-found", errorType: "cancel", text: "no such room" },
    );
    expect(stanzaErrorContext(rejection)).toEqual({
      condition: "item-not-found",
      errorType: "cancel",
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

  test("yields an empty context for strings and unshaped rejections", () => {
    expect(stanzaErrorContext("client is disconnected")).toEqual({});
    expect(stanzaErrorContext(new Error("boom"))).toEqual({});
    expect(stanzaErrorContext(null)).toEqual({});
  });
});
