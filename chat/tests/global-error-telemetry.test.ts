import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { App } from "vue";
import { createSSRApp } from "vue";
import {
  __setFaroForTesting,
  handleUnhandledRejectionEvent,
  handleWindowErrorEvent,
  installGlobalErrorTelemetry,
} from "../src/lib/telemetry";
import configureVueApp from "../src/vue-app";

function createFaroStub() {
  const errors: Array<{
    error: Error;
    options?: { type?: string; context?: Record<string, string> };
  }> = [];
  return {
    errors,
    api: {
      pushEvent: () => {},
      pushMeasurement: () => {},
      pushError: (
        error: Error,
        options?: { type?: string; context?: Record<string, string> },
      ) => {
        errors.push({ error, options });
      },
    },
  };
}

type FaroStub = ReturnType<typeof createFaroStub>;

let stub: FaroStub;

beforeEach(() => {
  __setFaroForTesting(null);
  stub = createFaroStub();
  __setFaroForTesting(stub as unknown as Parameters<typeof __setFaroForTesting>[0]);
});

afterEach(() => {
  __setFaroForTesting(null);
});

describe("handleWindowErrorEvent", () => {
  test("drops ResizeObserver undelivered-notification errors", () => {
    handleWindowErrorEvent({
      error: new Error("ResizeObserver loop completed with undelivered notifications."),
    });

    expect(stub.errors).toHaveLength(0);
  });

  test("drops WebKit ResizeObserver loop-limit errors", () => {
    handleWindowErrorEvent({
      error: new Error("ResizeObserver loop limit exceeded"),
    });

    expect(stub.errors).toHaveLength(0);
  });

  test("drops benign ResizeObserver messages without an Error object", () => {
    handleWindowErrorEvent({
      message: "ResizeObserver loop completed with undelivered notifications.",
    });

    expect(stub.errors).toHaveLength(0);
  });

  test("drops benign ResizeObserver messages carried as a string error", () => {
    handleWindowErrorEvent({
      error: "ResizeObserver loop limit exceeded",
    });

    expect(stub.errors).toHaveLength(0);
  });

  test("pushes a sanitized window-error to Faro", () => {
    handleWindowErrorEvent({
      error: new Error("stream broke for alice@example.com/desktop"),
    });

    expect(stub.errors).toHaveLength(1);
    const pushed = stub.errors[0]!;
    expect(pushed.options?.type).toBe("window-error");
    expect(pushed.error.message).toBe("stream broke for :jid");
    expect(pushed.error.message).not.toContain("alice@example.com");
  });

  test("falls back to the event message when no Error object is attached", () => {
    handleWindowErrorEvent({ message: "Script error for bob@example.com" });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0]!.error.message).toBe("Script error for :jid");
  });

  test("dedupes an identical error flood into a single push", () => {
    const boom = new Error("same boom");
    handleWindowErrorEvent({ error: boom });
    handleWindowErrorEvent({ error: boom });
    handleWindowErrorEvent({ error: boom });

    expect(stub.errors).toHaveLength(1);
  });

  test("distinct errors are each reported", () => {
    handleWindowErrorEvent({ error: new Error("first boom") });
    handleWindowErrorEvent({ error: new Error("second boom") });

    expect(stub.errors).toHaveLength(2);
  });
});

describe("handleUnhandledRejectionEvent", () => {
  test("pushes a sanitized unhandled-rejection to Faro", () => {
    handleUnhandledRejectionEvent({
      reason: new Error("fetch failed: https://x.example/ws?session_id=abc123&api_key=topsecret"),
    });

    expect(stub.errors).toHaveLength(1);
    const pushed = stub.errors[0]!;
    expect(pushed.options?.type).toBe("unhandled-rejection");
    expect(pushed.error.message).not.toContain("topsecret");
    expect(pushed.error.message).not.toContain("abc123");
  });

  test("stringifies non-Error rejection reasons", () => {
    handleUnhandledRejectionEvent({ reason: "plain string reason" });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0]!.error.message).toBe("plain string reason");
  });
});

describe("installGlobalErrorTelemetry", () => {
  test("registers window error and unhandledrejection listeners exactly once", () => {
    const listeners = new Map<string, Array<(event: unknown) => void>>();
    const windowStub = {
      addEventListener: (type: string, listener: (event: unknown) => void) => {
        const bucket = listeners.get(type) ?? [];
        bucket.push(listener);
        listeners.set(type, bucket);
      },
    };
    const originalWindow = globalThis.window;
    (globalThis as { window: unknown }).window = windowStub;
    try {
      installGlobalErrorTelemetry();
      installGlobalErrorTelemetry();

      expect(listeners.get("error")).toHaveLength(1);
      expect(listeners.get("unhandledrejection")).toHaveLength(1);

      listeners.get("error")![0]!({ error: new Error("listener boom for carol@example.com") });
      listeners.get("unhandledrejection")![0]!({ reason: new Error("rejection boom") });

      expect(stub.errors).toHaveLength(2);
      expect(stub.errors[0]!.error.message).toBe("listener boom for :jid");
      expect(stub.errors[1]!.options?.type).toBe("unhandled-rejection");
    } finally {
      if (originalWindow === undefined) {
        Reflect.deleteProperty(globalThis, "window");
      } else {
        (globalThis as { window: unknown }).window = originalWindow;
      }
    }
  });

  test("reinstalls listeners when the window instance changes (test window swaps, HMR re-init)", () => {
    function makeWindowStub() {
      const listeners = new Map<string, Array<(event: unknown) => void>>();
      return {
        listeners,
        stub: {
          addEventListener: (type: string, listener: (event: unknown) => void) => {
            const bucket = listeners.get(type) ?? [];
            bucket.push(listener);
            listeners.set(type, bucket);
          },
        },
      };
    }
    const first = makeWindowStub();
    const second = makeWindowStub();
    const originalWindow = globalThis.window;
    try {
      (globalThis as { window: unknown }).window = first.stub;
      installGlobalErrorTelemetry();
      expect(first.listeners.get("error")).toHaveLength(1);

      // A different window object (fresh stub, HMR-recreated window)
      // must get its own listeners — the guard is per-window, not a
      // process-lifetime latch.
      (globalThis as { window: unknown }).window = second.stub;
      installGlobalErrorTelemetry();
      expect(second.listeners.get("error")).toHaveLength(1);
      expect(second.listeners.get("unhandledrejection")).toHaveLength(1);

      // Same window again stays idempotent.
      installGlobalErrorTelemetry();
      expect(second.listeners.get("error")).toHaveLength(1);
    } finally {
      if (originalWindow === undefined) {
        Reflect.deleteProperty(globalThis, "window");
      } else {
        (globalThis as { window: unknown }).window = originalWindow;
      }
    }
  });
});

describe("vue app errorHandler", () => {
  function appWithHandler(): App {
    const app = createSSRApp({ render: () => null });
    configureVueApp(app);
    return app;
  }

  test("funnels render errors through sanitized reportError", () => {
    const app = appWithHandler();
    expect(app.config.errorHandler).toBeInstanceOf(Function);

    app.config.errorHandler!(
      new Error("render exploded for dave@example.com/phone"),
      null,
      "render function",
    );

    expect(stub.errors).toHaveLength(1);
    const pushed = stub.errors[0]!;
    expect(pushed.options?.type).toBe("vue-render-error");
    expect(pushed.error.message).toBe("render exploded for :jid");
    expect(pushed.options?.context?.detail).toBe("render function");
  });

  test("includes the component name when the instance exposes one", () => {
    const app = appWithHandler();
    const instance = { $options: { name: "GifPicker" } };

    app.config.errorHandler!(
      new Error("boom"),
      instance as never,
      "render function",
    );

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0]!.options?.context?.component).toBe("GifPicker");
  });
});
