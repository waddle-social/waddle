import { nextTick, ref, watch, type Ref } from "vue";
import {
  getPinnedScrollTop,
  isTopPinnedScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";

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

  function refreshPinnedState(): boolean {
    recomputePinned();
    return isPinnedAtEdge.value;
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

  function cancelSettleLock() {
    disconnectSettleLock();
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
    cancelSettleLock,
    disconnect,
    refreshPinnedState,
    scrollToPinnedEdge,
    isPinnedAtEdge: isPinnedAtEdge as Readonly<Ref<boolean>>,
  };
}
