import { afterEach, describe, expect, test } from "bun:test";
import type { Span } from "@opentelemetry/api";
import {
  __setFaroForTesting,
  __setSpanFactoryForTesting,
  reportQueueDepthChange,
  reportSessionLifecycle,
  withSpan,
} from "../src/lib/telemetry";

afterEach(() => {
  __setFaroForTesting(null);
  __setSpanFactoryForTesting(null);
});

describe("telemetry failure containment", () => {
  test("drops throwing pushEvent and pushMeasurement calls", () => {
    __setFaroForTesting({
      api: {
        pushEvent: () => { throw new Error("event collector unavailable"); },
        pushMeasurement: () => { throw new Error("measurement collector unavailable"); },
      },
    } as never);

    expect(() => reportSessionLifecycle({ type: "fresh" })).not.toThrow();
    expect(() => reportQueueDepthChange({ kind: "dm", persisted: 1, inflight: 0 })).not.toThrow();
  });

  test("drops recursive emission from a re-entering pushEvent", () => {
    let pushes = 0;
    __setFaroForTesting({
      api: {
        pushEvent: () => {
          pushes += 1;
          reportSessionLifecycle({ type: "resumed" });
          throw new Error("event collector unavailable");
        },
      },
    } as never);

    expect(() => reportSessionLifecycle({ type: "fresh" })).not.toThrow();
    expect(pushes).toBe(1);
  });

  test("span setup failure falls back to the protocol callback", async () => {
    __setFaroForTesting({ api: {} } as never);
    __setSpanFactoryForTesting(() => { throw new Error("span setup failed"); });

    await expect(withSpan({ kind: "xmpp-connect" }, async () => "connected")).resolves.toBe("connected");
  });

  test("span status and end failures do not mask a successful callback", async () => {
    __setFaroForTesting({ api: {} } as never);
    __setSpanFactoryForTesting(() => ({
      setStatus: () => { throw new Error("status failed"); },
      end: () => { throw new Error("end failed"); },
    } as unknown as Span));

    await expect(withSpan({ kind: "room-switch" }, async () => 42)).resolves.toBe(42);
  });

  test("span status, exception and end failures preserve the callback error", async () => {
    __setFaroForTesting({ api: {} } as never);
    __setSpanFactoryForTesting(() => ({
      setStatus: () => { throw new Error("status failed"); },
      recordException: () => { throw new Error("exception failed"); },
      end: () => { throw new Error("end failed"); },
    } as unknown as Span));
    const protocolError = new Error("protocol failed");

    await expect(withSpan(
      { kind: "initial-render", conversation: "room" },
      async () => { throw protocolError; },
    )).rejects.toBe(protocolError);
  });

  test("the telemetry module has no browser persistence calls", async () => {
    const source = await Bun.file(new URL("../src/lib/telemetry.ts", import.meta.url)).text();

    expect(source).not.toMatch(/\b(?:localStorage|sessionStorage|indexedDB|caches)\b/);
  });
});
