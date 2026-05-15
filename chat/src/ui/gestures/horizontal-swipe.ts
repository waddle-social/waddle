import { getCurrentScope, onScopeDispose, ref, type Ref } from "vue";

interface UseHorizontalSwipeOptions {
  /**
   * Distance in CSS pixels the finger has to travel before the swipe is
   * "armed" — we start translating the message and showing the hint icon.
   * Below this we stay inert so a wobbly tap or a vertical scroll doesn't
   * jitter the row. Default 8.
   */
  startThreshold?: number;
  /**
   * Distance in CSS pixels the finger has to travel before the gesture
   * commits on release. Below this the row snaps back without firing.
   * Default 72.
   */
  commitThreshold?: number;
  /**
   * Hard cap on translate distance during the drag, so the message can't
   * be flung off-screen. Default 120.
   */
  maxTranslate?: number;
  /**
   * Ratio of vertical-to-horizontal motion that disqualifies the gesture as
   * a swipe (treat as a vertical scroll instead). Default 1.2.
   */
  verticalLockRatio?: number;
  /** Fires when the user releases past `commitThreshold` to the left (RTL). */
  onSwipeLeft?: () => void;
  /** Fires when the user releases past `commitThreshold` to the right (LTR). */
  onSwipeRight?: () => void;
}

interface UseHorizontalSwipeReturn {
  handlers: {
    onPointerdown: (event: PointerEvent) => void;
    onPointermove: (event: PointerEvent) => void;
    onPointerup: (event: PointerEvent) => void;
    onPointercancel: (event: PointerEvent) => void;
    onPointerleave: (event: PointerEvent) => void;
  };
  /**
   * Live horizontal drag offset in CSS pixels, clamped to `[-maxTranslate,
   * maxTranslate]`. 0 when idle. Negative = swiping toward the left
   * (RTL); positive = toward the right (LTR). Components bind this to
   * `transform: translateX(...)` and to the opacity of the hint icons.
   */
  translateX: Ref<number>;
  /** True from `startThreshold` crossing until release. */
  isSwiping: Ref<boolean>;
  /** True when `|translateX| >= commitThreshold`. Hint icon goes "armed". */
  isArmed: Ref<boolean>;
  /** Sign of the current swipe direction (-1 left, +1 right, 0 idle). */
  direction: Ref<-1 | 0 | 1>;
  cancel: () => void;
}

/**
 * Touch-only horizontal swipe detector built on Pointer Events, sibling to
 * `useLongPress`. Vertical motion abandons the gesture so the page can
 * scroll. Mouse and pen pointers are ignored (desktop has the hover
 * toolbar). Commit on release; snap back otherwise.
 *
 * Composables call this alongside the long-press handlers — they're
 * mutually exclusive in practice (long-press cancels on movement > 10px;
 * the swipe arms past 8px) and the consumer wires both pointer handler
 * sets onto the same element via small forwarders.
 */
export function useHorizontalSwipe(
  options: UseHorizontalSwipeOptions,
): UseHorizontalSwipeReturn {
  const {
    startThreshold = 8,
    commitThreshold = 72,
    maxTranslate = 120,
    verticalLockRatio = 1.2,
    onSwipeLeft,
    onSwipeRight,
  } = options;

  const translateX = ref(0);
  const isSwiping = ref(false);
  const isArmed = ref(false);
  const direction = ref<-1 | 0 | 1>(0);

  let activePointerId: number | null = null;
  let startX = 0;
  let startY = 0;
  let lockedAxis: "horizontal" | "vertical" | null = null;

  function reset() {
    activePointerId = null;
    lockedAxis = null;
    isSwiping.value = false;
    isArmed.value = false;
    direction.value = 0;
    translateX.value = 0;
  }

  function onPointerdown(event: PointerEvent) {
    if (event.pointerType !== "touch") return;
    if (activePointerId !== null) return;
    activePointerId = event.pointerId;
    startX = event.clientX;
    startY = event.clientY;
    lockedAxis = null;
    translateX.value = 0;
    isSwiping.value = false;
    isArmed.value = false;
    direction.value = 0;
  }

  function onPointermove(event: PointerEvent) {
    if (event.pointerId !== activePointerId) return;
    const dx = event.clientX - startX;
    const dy = event.clientY - startY;

    if (lockedAxis === null) {
      const adx = Math.abs(dx);
      const ady = Math.abs(dy);
      if (adx < startThreshold && ady < startThreshold) return;
      // Whichever axis dominates first locks the gesture in for the rest
      // of this drag. Once locked vertical we never become a swipe.
      if (ady > adx * verticalLockRatio) {
        lockedAxis = "vertical";
        return;
      }
      lockedAxis = "horizontal";
      isSwiping.value = true;
    }

    if (lockedAxis !== "horizontal") return;

    const clamped = Math.max(-maxTranslate, Math.min(maxTranslate, dx));
    translateX.value = clamped;
    direction.value = clamped < 0 ? -1 : clamped > 0 ? 1 : 0;
    isArmed.value = Math.abs(clamped) >= commitThreshold;
  }

  function onPointerup(event: PointerEvent) {
    if (event.pointerId !== activePointerId) return;
    if (lockedAxis === "horizontal" && isArmed.value) {
      if (direction.value === -1) {
        onSwipeLeft?.();
      } else if (direction.value === 1) {
        onSwipeRight?.();
      }
    }
    reset();
  }

  function onPointercancel(event: PointerEvent) {
    if (event.pointerId !== activePointerId) return;
    reset();
  }

  function onPointerleave(event: PointerEvent) {
    if (event.pointerId !== activePointerId) return;
    reset();
  }

  function cancel() {
    reset();
  }

  if (getCurrentScope()) {
    onScopeDispose(cancel);
  }

  return {
    handlers: {
      onPointerdown,
      onPointermove,
      onPointerup,
      onPointercancel,
      onPointerleave,
    },
    translateX,
    isSwiping,
    isArmed,
    direction,
    cancel,
  };
}
