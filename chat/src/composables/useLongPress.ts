import { getCurrentScope, onScopeDispose, ref, type Ref } from "vue";

interface UseLongPressOptions {
  /** Delay in ms before the press fires. Default 500. */
  delay?: number;
  /** Max pointer movement in CSS pixels before the press cancels. Default 10. */
  moveThreshold?: number;
  /** Called when the press crosses the delay threshold without being cancelled. */
  onLongPress: (event: PointerEvent) => void;
}

interface UseLongPressReturn {
  handlers: {
    onPointerdown: (event: PointerEvent) => void;
    onPointermove: (event: PointerEvent) => void;
    onPointerup: (event: PointerEvent) => void;
    onPointercancel: (event: PointerEvent) => void;
    onPointerleave: (event: PointerEvent) => void;
    onContextmenu: (event: MouseEvent) => void;
  };
  isPressing: Ref<boolean>;
  cancel: () => void;
}

/**
 * Touch-only long-press detector built on Pointer Events.
 *
 * Fires `onLongPress` after {@link UseLongPressOptions.delay}ms of a stationary
 * `pointerType === "touch"` press. Movement beyond `moveThreshold` cancels.
 * Mouse and pen pointers are ignored so desktop hover behaviour is untouched.
 * The synthetic click that follows a long-press is swallowed at the window
 * level so buttons underneath the press don't double-fire.
 */
export function useLongPress(options: UseLongPressOptions): UseLongPressReturn {
  const { delay = 500, moveThreshold = 10, onLongPress } = options;

  const isPressing = ref(false);
  let timer: ReturnType<typeof setTimeout> | null = null;
  let startX = 0;
  let startY = 0;
  let activePointerId: number | null = null;
  let fired = false;
  let capturedTarget: Element | null = null;

  function clearTimer() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function releaseCapture() {
    if (capturedTarget && activePointerId !== null) {
      try {
        capturedTarget.releasePointerCapture(activePointerId);
      } catch {
        // Ignore — some browsers throw when capture was never granted.
      }
    }
    capturedTarget = null;
  }

  function reset() {
    clearTimer();
    releaseCapture();
    isPressing.value = false;
    activePointerId = null;
    fired = false;
  }

  function swallowNextClick() {
    if (
      typeof window === "undefined"
      || typeof window.addEventListener !== "function"
      || typeof window.removeEventListener !== "function"
    ) {
      return;
    }
    const handler = (event: MouseEvent) => {
      event.stopPropagation();
      event.preventDefault();
    };
    window.addEventListener("click", handler, { capture: true, once: true });
    // Safety net: if no click arrives, remove after a tick.
    setTimeout(() => {
      window.removeEventListener("click", handler, { capture: true } as EventListenerOptions);
    }, 400);
  }

  function onPointerdown(event: PointerEvent) {
    if (event.pointerType !== "touch") return;
    // Ignore multi-touch: only track the first pointer.
    if (activePointerId !== null) return;

    activePointerId = event.pointerId;
    startX = event.clientX;
    startY = event.clientY;
    isPressing.value = true;
    fired = false;

    timer = setTimeout(() => {
      if (activePointerId === null) return;
      fired = true;
      timer = null;

      const target = event.target;
      if (
        target &&
        typeof (target as { setPointerCapture?: unknown }).setPointerCapture === "function"
      ) {
        try {
          (target as Element).setPointerCapture(event.pointerId);
          capturedTarget = target as Element;
        } catch {
          // Ignore — capture is a best-effort optimisation.
        }
      }

      if (typeof navigator !== "undefined" && typeof navigator.vibrate === "function") {
        try {
          navigator.vibrate(10);
        } catch {
          // Some browsers throw when vibration is blocked by user settings.
        }
      }

      swallowNextClick();
      onLongPress(event);
    }, delay);
  }

  function onPointermove(event: PointerEvent) {
    if (event.pointerId !== activePointerId) return;
    if (fired) return;
    const dx = event.clientX - startX;
    const dy = event.clientY - startY;
    if (dx * dx + dy * dy > moveThreshold * moveThreshold) {
      reset();
    }
  }

  function onPointerup(event: PointerEvent) {
    if (event.pointerId !== activePointerId) return;
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

  function onContextmenu(event: MouseEvent) {
    // Touch long-press on iOS Safari synthesises a contextmenu event we want
    // to suppress when we handled the gesture ourselves. Leaving mouse
    // right-click alone keeps desktop devtools / native menus usable.
    if (isPressing.value || fired) {
      event.preventDefault();
    }
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
      onContextmenu,
    },
    isPressing,
    cancel,
  };
}
