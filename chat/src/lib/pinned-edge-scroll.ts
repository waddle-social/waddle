import { nextTick, watch, type Ref } from "vue";
import {
  getPinnedScrollTop,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";

type EdgeScrollFn = (mode: ScrollDirectionMode) => boolean | Promise<boolean>;
type ScrollTarget = {
  element: HTMLElement | null;
  mode: ScrollDirectionMode;
  token: number;
  virtualScroll: EdgeScrollFn | null;
};

export function createPinnedEdgeScroller(options: {
  element: Ref<HTMLElement | null>;
  mode: Ref<ScrollDirectionMode>;
  virtualScroll?: Ref<EdgeScrollFn | null>;
}) {
  let settleObserver: ResizeObserver | null = null;
  let settleTimer: ReturnType<typeof setTimeout> | null = null;
  let generation = 0;
  let settleGeneration = 0;

  function disconnectSettleLock() {
    generation++;
    if (settleTimer) {
      clearTimeout(settleTimer);
      settleTimer = null;
    }
    settleObserver?.disconnect();
    settleObserver = null;
  }

  function captureTarget(): ScrollTarget {
    return {
      element: options.element.value,
      mode: options.mode.value,
      token: generation,
      virtualScroll: options.virtualScroll?.value ?? null,
    };
  }

  async function pinTarget(target: ScrollTarget): Promise<boolean> {
    if (target.virtualScroll && await target.virtualScroll(target.mode)) {
      return target.token === generation;
    }
    if (target.token !== generation) return false;

    if (!target.element) return false;
    target.element.scrollTop = getPinnedScrollTop(target.element, target.mode);
    return true;
  }

  async function pinCurrent(token: number): Promise<boolean> {
    const target = captureTarget();
    if (target.virtualScroll && await target.virtualScroll(target.mode)) {
      return target.token === generation && token === generation;
    }
    if (target.token !== generation || token !== generation) return false;

    const currentElement = options.element.value;
    if (!currentElement) return false;
    currentElement.scrollTop = getPinnedScrollTop(currentElement, options.mode.value);
    return true;
  }

  function refreshSettleLock() {
    const el = options.element.value;
    if (!el || typeof ResizeObserver === "undefined") return;

    const target = captureTarget();
    settleGeneration = target.token;
    settleObserver?.disconnect();
    settleObserver = new ResizeObserver(() => {
      if (settleGeneration !== generation) return;
      void pinTarget(target);
    });
    settleObserver.observe(el);
    for (const child of Array.from(el.children)) {
      settleObserver.observe(child);
    }

    if (settleTimer) clearTimeout(settleTimer);
    settleTimer = setTimeout(disconnectSettleLock, 500);
  }

  async function scrollToPinnedEdge(optionsForScroll: { settle?: boolean } = {}) {
    const token = generation;
    await nextTick();
    await nextTick();
    if (token !== generation) return false;
    const didScroll = await pinCurrent(token);
    if (token !== generation) return false;
    if (didScroll && optionsForScroll.settle) {
      refreshSettleLock();
    }
    return didScroll;
  }

  watch(
    () => [options.element.value, options.virtualScroll?.value ?? null] as const,
    () => disconnectSettleLock(),
    { flush: "sync" },
  );

  return {
    disconnect: disconnectSettleLock,
    scrollToPinnedEdge,
  };
}
