import { describe, expect, test } from "bun:test";
import { nextTick, ref } from "vue";
import { createPinnedEdgeScroller } from "../src/lib/pinned-edge-scroll";

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
