import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { effectScope } from "vue";
import { useLongPress } from "../src/ui/gestures/long-press";

type PointerInit = {
  pointerId?: number;
  pointerType?: "touch" | "mouse" | "pen";
  clientX?: number;
  clientY?: number;
  target?: EventTarget | null;
};

function makePointerEvent(init: PointerInit = {}): PointerEvent {
  const {
    pointerId = 1,
    pointerType = "touch",
    clientX = 0,
    clientY = 0,
    target = null,
  } = init;
  // Happy-DOM/jsdom may not ship PointerEvent; fall back to a plain object
  // with the fields the composable actually reads.
  return {
    pointerId,
    pointerType,
    clientX,
    clientY,
    target,
    preventDefault() {},
    stopPropagation() {},
  } as unknown as PointerEvent;
}

async function waitMs(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

function stubBrowserGlobal(name: "window" | "navigator", value: unknown): () => void {
  const original = Object.getOwnPropertyDescriptor(globalThis, name);
  Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  return () => {
    if (original) Object.defineProperty(globalThis, name, original);
    else delete (globalThis as Record<string, unknown>)[name];
  };
}

describe("long-press browser globals", () => {
  for (const { label, descriptor } of [
    {
      label: "read-only data property",
      descriptor: { configurable: true, enumerable: true, writable: false, value: {} },
    },
    { label: "getter-only property", descriptor: { configurable: true, get: () => ({}) } },
    { label: "absent property", descriptor: undefined },
  ]) {
    test(`restores the exact ${label} after stubbing`, () => {
      const original = Object.getOwnPropertyDescriptor(globalThis, "navigator");
      try {
        delete (globalThis as Record<string, unknown>).navigator;
        if (descriptor) Object.defineProperty(globalThis, "navigator", descriptor);
        const before = Object.getOwnPropertyDescriptor(globalThis, "navigator");
        const navigator = { vibrate: mock(() => true) };
        const restore = stubBrowserGlobal("navigator", navigator);
        expect(globalThis.navigator).toBe(navigator);
        restore();
        expect(Object.getOwnPropertyDescriptor(globalThis, "navigator")).toEqual(before);
      } finally {
        if (original) Object.defineProperty(globalThis, "navigator", original);
        else delete (globalThis as Record<string, unknown>).navigator;
      }
    });
  }
});

describe("useLongPress", () => {
  // Node's globalThis needs a stub window for the click-swallow path.
  let originalWindow: Window & typeof globalThis;
  let restoreWindow: () => void;
  let restoreNavigator: () => void;
  const clickListeners: Array<(event: MouseEvent) => void> = [];

  beforeEach(() => {
    clickListeners.length = 0;
    originalWindow = globalThis.window;
    restoreWindow = stubBrowserGlobal("window", {
      addEventListener(type: string, listener: (event: MouseEvent) => void) {
        if (type === "click") clickListeners.push(listener);
      },
      removeEventListener(type: string, listener: (event: MouseEvent) => void) {
        if (type !== "click") return;
        const idx = clickListeners.indexOf(listener);
        if (idx >= 0) clickListeners.splice(idx, 1);
      },
    });
    restoreNavigator = stubBrowserGlobal("navigator", { vibrate: () => true });
  });

  afterEach(() => {
    restoreNavigator();
    restoreWindow();
  });

  test("fires after the delay when the finger stays still", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 20, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent({ clientX: 0, clientY: 0 }));
    });
    await waitMs(40);
    expect(onLongPress).toHaveBeenCalledTimes(1);
    scope.stop();
  });

  test("cancels when the pointer moves beyond the threshold", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 20, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent({ clientX: 0, clientY: 0 }));
      lp.handlers.onPointermove(makePointerEvent({ clientX: 50, clientY: 0 }));
    });
    await waitMs(40);
    expect(onLongPress).not.toHaveBeenCalled();
    scope.stop();
  });

  test("cancels when the pointer lifts before the delay", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 30, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent());
      lp.handlers.onPointerup(makePointerEvent());
    });
    await waitMs(50);
    expect(onLongPress).not.toHaveBeenCalled();
    scope.stop();
  });

  test("ignores non-touch pointer types", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 20, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent({ pointerType: "mouse" }));
      lp.handlers.onPointerdown(makePointerEvent({ pointerType: "pen", pointerId: 2 }));
    });
    await waitMs(40);
    expect(onLongPress).not.toHaveBeenCalled();
    scope.stop();
  });

  test("swallows the synthetic click that follows a long-press", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 20, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent());
    });
    await waitMs(40);
    expect(clickListeners).toHaveLength(1);
    let defaultPrevented = false;
    let propagationStopped = false;
    const fakeClick = {
      preventDefault() {
        defaultPrevented = true;
      },
      stopPropagation() {
        propagationStopped = true;
      },
    } as unknown as MouseEvent;
    clickListeners[0](fakeClick);
    expect(defaultPrevented).toBe(true);
    expect(propagationStopped).toBe(true);
    scope.stop();
  });

  test("safety cleanup removes the listener from the original window", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 20, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent());
    });
    await waitMs(40);
    expect(clickListeners).toHaveLength(1);

    globalThis.window = originalWindow;
    await waitMs(410);

    expect(clickListeners).toHaveLength(0);
    scope.stop();
  });

  test("cancel() aborts a pending press", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 30, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent());
      lp.cancel();
    });
    await waitMs(50);
    expect(onLongPress).not.toHaveBeenCalled();
    scope.stop();
  });

  test("unmounting the owning scope clears pending timers", async () => {
    const onLongPress = mock(() => {});
    const scope = effectScope();
    scope.run(() => {
      const lp = useLongPress({ delay: 30, moveThreshold: 10, onLongPress });
      lp.handlers.onPointerdown(makePointerEvent());
    });
    scope.stop();
    await waitMs(50);
    expect(onLongPress).not.toHaveBeenCalled();
  });
});
