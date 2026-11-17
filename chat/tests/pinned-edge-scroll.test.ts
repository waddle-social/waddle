import { describe, expect, test } from "bun:test";
import { nextTick, ref } from "vue";
import { createPinnedEdgeScroller, preserveScrollOnPrepend } from "../src/lib/pinned-edge-scroll";

class ScrollHarness {
  scrollTop = 0;
  scrollHeight = 1000;
  clientHeight = 200;
  children: Element[] = [];
  private readonly listeners = new Map<string, EventListener[]>();

  addEventListener(type: string, listener: EventListener) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: EventListener) {
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter((entry) => entry !== listener));
  }

  dispatchScroll() {
    const event = new Event("scroll");
    for (const listener of this.listeners.get("scroll") ?? []) listener(event);
  }
}

test("restores the current history position and ignores a stale page", async () => {
  const el = new ScrollHarness();
  const element = ref(el as unknown as HTMLElement);
  const mode = ref<"chat" | "social">("chat");
  el.scrollTop = 180;

  await preserveScrollOnPrepend(element, mode, () => true, () => {
    // New rows add 400 px and removing the loading row subtracts 60 px.
    el.scrollHeight += 340;
  });
  expect(el.scrollTop).toBe(520);

  const stalePage = preserveScrollOnPrepend(element, mode, () => false, () => {
    el.scrollHeight += 400;
  });
  await stalePage;
  expect(el.scrollTop).toBe(520);
});

describe("pinned edge scroller", () => {
  test("updates pinned-state reads synchronously on user scroll", () => {
    const el = new ScrollHarness();
    el.scrollTop = 800;
    const scroller = createPinnedEdgeScroller({
      element: ref(el as unknown as HTMLElement),
      mode: ref("chat"),
    });

    expect(scroller.isPinnedAtEdge.value).toBe(true);

    el.scrollTop = 100;
    el.dispatchScroll();

    expect(scroller.isPinnedAtEdge.value).toBe(false);
    scroller.disconnect();
  });

  test("programmatic scroll updates pinned state immediately", async () => {
    const el = new ScrollHarness();
    el.scrollTop = 100;
    const scroller = createPinnedEdgeScroller({
      element: ref(el as unknown as HTMLElement),
      mode: ref("chat"),
    });

    expect(scroller.isPinnedAtEdge.value).toBe(false);

    await scroller.scrollToPinnedEdge();

    expect(el.scrollTop).toBe(1000);
    expect(scroller.isPinnedAtEdge.value).toBe(true);
    scroller.disconnect();
  });

  test("shares an in-flight virtual pin and lets the timeline follow resizes", async () => {
    const originalObserver = globalThis.ResizeObserver;
    let observers = 0;
    globalThis.ResizeObserver = class {
      constructor() { observers++; }
      observe() {}
      unobserve() {}
      disconnect() {}
    };
    const el = new ScrollHarness();
    const pending = Promise.withResolvers<boolean>();
    let scrolls = 0;
    const scroller = createPinnedEdgeScroller({
      element: ref(el as unknown as HTMLElement),
      mode: ref("chat"),
      virtualScroll: ref(() => {
        scrolls++;
        return pending.promise;
      }),
    });

    try {
      const load = scroller.scrollToPinnedEdge({ settle: true });
      const mount = scroller.scrollToPinnedEdge({ settle: true });
      pending.resolve(true);
      expect(await Promise.all([load, mount])).toEqual([true, true]);
      expect(scrolls).toBe(1);
      expect(observers).toBe(0);
    } finally {
      scroller.disconnect();
      globalThis.ResizeObserver = originalObserver;
    }
  });

  test("ignores an earlier virtual pin after the scroll mode changes", async () => {
    const el = new ScrollHarness();
    const started = Promise.withResolvers<void>();
    const oldPin = Promise.withResolvers<boolean>();
    const mode = ref<"chat" | "social">("chat");
    const scroller = createPinnedEdgeScroller({
      element: ref(el as unknown as HTMLElement),
      mode,
      virtualScroll: ref((requestedMode: typeof mode.value) => {
        if (requestedMode === "social") return false;
        started.resolve();
        return oldPin.promise;
      }),
    });

    try {
      const stale = scroller.scrollToPinnedEdge();
      await started.promise;
      mode.value = "social";
      expect(await scroller.scrollToPinnedEdge()).toBe(true);
      oldPin.resolve(false);
      expect(await stale).toBe(false);
      expect(el.scrollTop).toBe(0);
    } finally {
      scroller.disconnect();
    }
  });

  test("temporary disconnect can reattach pinned-state tracking to a new element", async () => {
    const first = new ScrollHarness();
    first.scrollTop = 800;
    const second = new ScrollHarness();
    second.scrollTop = 800;
    const element = ref(first as unknown as HTMLElement);
    const scroller = createPinnedEdgeScroller({
      element,
      mode: ref("chat"),
    });

    scroller.disconnect();
    element.value = second as unknown as HTMLElement;
    await nextTick();

    second.scrollTop = 100;
    second.dispatchScroll();

    expect(scroller.isPinnedAtEdge.value).toBe(false);
    scroller.disconnect();
  });

  test("cancelling the settle lock keeps same-element scroll tracking attached", () => {
    const el = new ScrollHarness();
    el.scrollTop = 800;
    const scroller = createPinnedEdgeScroller({
      element: ref(el as unknown as HTMLElement),
      mode: ref("chat"),
    });

    scroller.cancelSettleLock();
    el.scrollTop = 100;
    el.dispatchScroll();

    expect(scroller.isPinnedAtEdge.value).toBe(false);
    scroller.disconnect();
  });

  test("refreshPinnedState synchronously recomputes after programmatic scroll", () => {
    const el = new ScrollHarness();
    el.scrollTop = 800;
    const scroller = createPinnedEdgeScroller({
      element: ref(el as unknown as HTMLElement),
      mode: ref("chat"),
    });

    el.scrollTop = 100;

    expect(scroller.refreshPinnedState()).toBe(false);
    expect(scroller.isPinnedAtEdge.value).toBe(false);
    scroller.disconnect();
  });
});
