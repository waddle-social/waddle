import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { context, trace, type Span } from "@opentelemetry/api";
import {
  __setFaroForTesting,
  reportError,
  reportMessageAcked,
  withSpan,
} from "../src/lib/telemetry";

const faroStub = {
  api: {
    pushEvent: () => undefined,
    pushMeasurement: () => undefined,
    pushError: () => undefined,
  },
};

beforeEach(() => {
  __setFaroForTesting(faroStub as never);
});

afterEach(() => {
  __setFaroForTesting(null);
});

function throwingSpan(): Span {
  return {
    setStatus: () => {
      throw new Error("setStatus failed");
    },
    recordException: () => {
      throw new Error("recordException failed");
    },
    end: () => {
      throw new Error("end failed");
    },
  } as unknown as Span;
}

describe("observe-only telemetry boundary", () => {
  test("runs the core callback once when span creation fails", async () => {
    const getTracer = spyOn(trace, "getTracer").mockReturnValue({
      startSpan: () => {
        throw new Error("startSpan failed");
      },
    } as never);
    let calls = 0;
    try {
      await expect(withSpan("test", {}, async () => {
        calls += 1;
        return "product-result";
      })).resolves.toBe("product-result");
      expect(calls).toBe(1);
    } finally {
      getTracer.mockRestore();
    }
  });

  test("ignores span failures without changing success or product errors", async () => {
    const getTracer = spyOn(trace, "getTracer").mockReturnValue({
      startSpan: () => throwingSpan(),
    } as never);
    try {
      let successCalls = 0;
      await expect(withSpan("test", {}, async () => {
        successCalls += 1;
        return 42;
      })).resolves.toBe(42);
      expect(successCalls).toBe(1);

      const productError = new Error("product failed");
      let failureCalls = 0;
      await expect(withSpan("test", {}, async () => {
        failureCalls += 1;
        throw productError;
      })).rejects.toBe(productError);
      expect(failureCalls).toBe(1);
    } finally {
      getTracer.mockRestore();
    }
  });

  test("does not retry when the context manager throws after invoking the callback", async () => {
    const getTracer = spyOn(trace, "getTracer").mockReturnValue({
      startSpan: () => throwingSpan(),
    } as never);
    const withContext = spyOn(context, "with").mockImplementation(((_active, callback) => {
      callback();
      throw new Error("context manager failed after callback");
    }) as never);
    let calls = 0;
    try {
      await expect(withSpan("test", {}, async () => {
        calls += 1;
        return "still-authoritative";
      })).resolves.toBe("still-authoritative");
      expect(calls).toBe(1);
    } finally {
      withContext.mockRestore();
      getTracer.mockRestore();
    }
  });

  test("ignores an unrelated never-settling context-manager return", async () => {
    const getTracer = spyOn(trace, "getTracer").mockReturnValue({
      startSpan: () => throwingSpan(),
    } as never);
    const neverSettles = new Promise<never>(() => undefined);
    const withContext = spyOn(context, "with").mockImplementation(((_active, callback) => {
      callback();
      return neverSettles;
    }) as never);
    let calls = 0;
    try {
      const result = Promise.race([
        withSpan("test", {}, async () => {
          calls += 1;
          return "product-result";
        }),
        new Promise<never>((_resolve, reject) => {
          setTimeout(() => reject(new Error("withSpan trusted the context return")), 100);
        }),
      ]);
      await expect(result).resolves.toBe("product-result");
      expect(calls).toBe(1);
    } finally {
      withContext.mockRestore();
      getTracer.mockRestore();
    }
  });

  test("late context callbacks reuse the directly-started execution", async () => {
    const getTracer = spyOn(trace, "getTracer").mockReturnValue({
      startSpan: () => throwingSpan(),
    } as never);
    let lateCallback: (() => Promise<unknown>) | undefined;
    const withContext = spyOn(context, "with").mockImplementation(((_active, callback) => {
      lateCallback = callback;
      return undefined;
    }) as never);
    let releaseProduct!: () => void;
    const productCanFinish = new Promise<void>((resolve) => {
      releaseProduct = resolve;
    });
    let calls = 0;
    try {
      const result = withSpan("test", {}, async () => {
        calls += 1;
        await productCanFinish;
        return "product-result";
      });
      expect(lateCallback).toBeFunction();
      void lateCallback?.();
      void lateCallback?.();
      releaseProduct();
      await expect(result).resolves.toBe("product-result");
      expect(calls).toBe(1);
    } finally {
      withContext.mockRestore();
      getTracer.mockRestore();
    }
  });

  test("contains SDK and hostile payload failures inside report helpers", () => {
    __setFaroForTesting({
      api: {
        pushEvent: () => {
          throw new Error("event transport failed");
        },
        pushMeasurement: () => {
          throw new Error("measurement transport failed");
        },
        pushError: () => {
          throw new Error("error transport failed");
        },
      },
    } as never);
    const hostileContext = new Proxy(
      { recoverable: true },
      { ownKeys: () => { throw new Error("context inspection failed"); } },
    );
    const hostileAck = new Proxy(
      { id: "message", kind: "dm" as const, latencyMs: 10 },
      { get: () => { throw new Error("payload inspection failed"); } },
    );

    expect(() => reportError("http.fetch", new Error("product"), hostileContext)).not.toThrow();
    expect(() => reportError("http.fetch", new Error("product"))).not.toThrow();
    expect(() => reportMessageAcked(hostileAck)).not.toThrow();
  });
});
