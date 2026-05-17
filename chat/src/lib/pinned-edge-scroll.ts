import { nextTick, ref, watch, type Ref } from "vue";
import {
  getPinnedScrollTop,
  isTopPinnedScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import { debugScroll } from "@/lib/debug-scroll";

const PINNED_EDGE_TOLERANCE_PX = 64;

type EdgeScrollFn = (mode: ScrollDirectionMode) => boolean | Promise<boolean>;
type ScrollTarget = {
  element: HTMLElement | null;
  mode: ScrollDirectionMode;
  token: number;
  virtualScroll: EdgeScrollFn | null;
};

function computeIsPinned(el: HTMLElement, mode: ScrollDirectionMode): boolean {
  const scrollTop = typeof el.scrollTop === "number" ? el.scrollTop : 0;
  if (isTopPinnedScrollDirection(mode)) {
    return scrollTop <= PINNED_EDGE_TOLERANCE_PX;
  }
  const scrollHeight = typeof el.scrollHeight === "number" ? el.scrollHeight : 0;
  const clientHeight = typeof el.clientHeight === "number" ? el.clientHeight : 0;
  const distanceFromBottom = scrollHeight - scrollTop - clientHeight;
  return distanceFromBottom <= PINNED_EDGE_TOLERANCE_PX;
}

export function createPinnedEdgeScroller(options: {
  element: Ref<HTMLElement | null>;
  mode: Ref<ScrollDirectionMode>;
  virtualScroll?: Ref<EdgeScrollFn | null>;
}) {
  let settleObserver: ResizeObserver | null = null;
  let settleTimer: ReturnType<typeof setTimeout> | null = null;
  let generation = 0;
  let settleGeneration = 0;

  const isPinnedAtEdge = ref(true);

  let trackedElement: HTMLElement | null = null;
  let scrollListener: (() => void) | null = null;

  function recomputePinned() {
    const el = options.element.value;
    if (!el) {
      if (isPinnedAtEdge.value !== true) isPinnedAtEdge.value = true;
      return;
    }
    const next = computeIsPinned(el, options.mode.value);
    if (isPinnedAtEdge.value !== next) isPinnedAtEdge.value = next;
  }

  function attachScrollListener() {
    const el = options.element.value;
    if (el === trackedElement) return;
    if (trackedElement && scrollListener && typeof trackedElement.removeEventListener === "function") {
      trackedElement.removeEventListener("scroll", scrollListener);
    }
    trackedElement = el;
    scrollListener = null;
    if (!el) {
      isPinnedAtEdge.value = true;
      return;
    }
    if (typeof el.addEventListener !== "function") {
      recomputePinned();
      return;
    }
    scrollListener = () => recomputePinned();
    el.addEventListener("scroll", scrollListener, { passive: true });
    recomputePinned();
  }

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
      recomputePinned();
      return target.token === generation;
    }
    if (target.token !== generation) return false;

    if (!target.element) return false;
    target.element.scrollTop = getPinnedScrollTop(target.element, target.mode);
    recomputePinned();
    return true;
  }

  async function pinCurrent(token: number): Promise<boolean> {
    const target = captureTarget();
    if (target.virtualScroll && await target.virtualScroll(target.mode)) {
      recomputePinned();
      return target.token === generation && token === generation;
    }
    if (target.token !== generation || token !== generation) return false;

    const currentElement = options.element.value;
    if (!currentElement) return false;
    currentElement.scrollTop = getPinnedScrollTop(currentElement, options.mode.value);
    recomputePinned();
    return true;
  }

  function refreshSettleLock() {
    const el = options.element.value;
    if (!el || typeof ResizeObserver === "undefined") return;

    const target = captureTarget();
    settleGeneration = target.token;
    settleObserver?.disconnect();
    const settleStartedAt = typeof performance !== "undefined" ? performance.now() : Date.now();
    let resizeEventCount = 0;
    settleObserver = new ResizeObserver(() => {
      if (settleGeneration !== generation) {
        debugScroll("pinned-edge.settleObserver: stale fire, drop", {
          settleGeneration,
          generation,
        });
        return;
      }
      resizeEventCount++;
      const elapsed = (typeof performance !== "undefined" ? performance.now() : Date.now()) - settleStartedAt;
      debugScroll("pinned-edge.settleObserver: fire -> pinTarget", {
        elapsedMs: Math.round(elapsed),
        eventCount: resizeEventCount,
        scrollTop: el.scrollTop,
        scrollHeight: el.scrollHeight,
        clientHeight: el.clientHeight,
        gapFromBottom: el.scrollHeight - el.scrollTop - el.clientHeight,
      });
      void pinTarget(target);
    });
    settleObserver.observe(el);
    let childCount = 0;
    for (const child of Array.from(el.children)) {
      settleObserver.observe(child);
      childCount++;
    }
    debugScroll("pinned-edge.refreshSettleLock: observer armed", {
      childCount,
      windowMs: 500,
      generation,
    });

    if (settleTimer) clearTimeout(settleTimer);
    settleTimer = setTimeout(() => {
      const elapsed = (typeof performance !== "undefined" ? performance.now() : Date.now()) - settleStartedAt;
      debugScroll("pinned-edge.settleTimer: expired", {
        elapsedMs: Math.round(elapsed),
        resizeEvents: resizeEventCount,
        scrollTop: el.scrollTop,
        scrollHeight: el.scrollHeight,
        gapFromBottom: el.scrollHeight - el.scrollTop - el.clientHeight,
      });
      disconnectSettleLock();
    }, 500);
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
    () => {
      disconnectSettleLock();
      attachScrollListener();
    },
    { flush: "sync", immediate: true },
  );

  watch(() => options.mode.value, () => recomputePinned());

  function disconnect() {
    disconnectSettleLock();
    if (trackedElement && scrollListener && typeof trackedElement.removeEventListener === "function") {
      trackedElement.removeEventListener("scroll", scrollListener);
    }
    trackedElement = null;
    scrollListener = null;
  }

  return {
    disconnect,
    scrollToPinnedEdge,
    isPinnedAtEdge: isPinnedAtEdge as Readonly<Ref<boolean>>,
  };
}
