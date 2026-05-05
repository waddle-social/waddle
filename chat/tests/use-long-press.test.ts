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

describe("useLongPress", () => {
  // Node's globalThis needs a stub window for the click-swallow path.
  const originalWindow = globalThis.window;
  const originalNavigator = globalThis.navigator;
  const clickListeners: Array<(event: MouseEvent) => void> = [];

  beforeEach(() => {
    clickListeners.length = 0;
    globalThis.window = {
      addEventListener(type: string, listener: (event: MouseEvent) => void) {
        if (type === "click") clickListeners.push(listener);
      },
      removeEventListener(type: string, listener: (event: MouseEvent) => void) {
        if (type !== "click") return;
        const idx = clickListeners.indexOf(listener);
        if (idx >= 0) clickListeners.splice(idx, 1);
      },
    } as unknown as Window & typeof globalThis;
    globalThis.navigator = { vibrate: () => true } as unknown as Navigator;
  });

  afterEach(() => {
    globalThis.window = originalWindow;
    globalThis.navigator = originalNavigator;
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
