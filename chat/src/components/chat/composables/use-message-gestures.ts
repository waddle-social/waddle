import { useHorizontalSwipe } from "@/ui/gestures/horizontal-swipe";
import { useLongPress } from "@/ui/gestures/long-press";

/** Movement in CSS pixels beyond which a touch is no longer a tap. */
const TAP_SLOP = 10;

/**
 * Touch gestures on a message card: long-press opens the action sheet,
 * horizontal swipes trigger thread / reply. Both gesture recognizers see
 * every pointer event so a drag that starts as a potential long-press can
 * still resolve as a swipe (and vice versa cancel each other).
 *
 * Also records whether the latest single-finger touch ended as a
 * stationary tap, so the row's click handler can tell a plain tap from a
 * click that follows a swipe, a scroll, a long press or a multi-touch
 * gesture. A click a browser suppresses (e.g. a tap that stops momentum
 * scrolling) never reaches that handler, so it opens nothing.
 */
export function useMessageGestures(input: {
  onLongPress: () => void;
  /** Right-to-left drag opens (or enters) the thread for this message. */
  onSwipeLeft: () => void;
  /** Left-to-right drag fills the composer reply chip targeting this
   * message — same path as the toolbar reply button. */
  onSwipeRight: () => void;
}) {
  // The first touch pointer of the current press; null when none is down.
  let tapPointerId: number | null = null;
  let tapDisqualified = false;
  let tapStartX = 0;
  let tapStartY = 0;
  // Outcome of the last completed press, consumed by the next click.
  let lastPressWasTap = false;

  const longPress = useLongPress({
    onLongPress: () => {
      tapDisqualified = true;
      input.onLongPress();
    },
  });

  const swipe = useHorizontalSwipe({
    onSwipeLeft: input.onSwipeLeft,
    onSwipeRight: input.onSwipeRight,
  });

  function onPointerdown(event: PointerEvent) {
    if (tapPointerId !== null) {
      // A second finger: this press is a multi-touch gesture, not a tap.
      tapDisqualified = true;
    } else {
      lastPressWasTap = false;
      if (event.pointerType === "touch") {
        tapPointerId = event.pointerId;
        tapDisqualified = false;
        tapStartX = event.clientX;
        tapStartY = event.clientY;
      }
    }
    swipe.handlers.onPointerdown(event);
    longPress.handlers.onPointerdown(event);
  }
  function onPointermove(event: PointerEvent) {
    if (event.pointerId === tapPointerId && !tapDisqualified) {
      const dx = event.clientX - tapStartX;
      const dy = event.clientY - tapStartY;
      if (dx * dx + dy * dy > TAP_SLOP * TAP_SLOP) tapDisqualified = true;
    }
    swipe.handlers.onPointermove(event);
    longPress.handlers.onPointermove(event);
  }
  function onPointerup(event: PointerEvent) {
    if (event.pointerId === tapPointerId) {
      lastPressWasTap = !tapDisqualified;
      tapPointerId = null;
    }
    swipe.handlers.onPointerup(event);
    longPress.handlers.onPointerup(event);
  }
  function onPointercancel(event: PointerEvent) {
    // The browser took the pointer (scroll, pinch): whatever follows is not a tap.
    if (event.pointerId === tapPointerId) {
      lastPressWasTap = false;
      tapPointerId = null;
    }
    swipe.handlers.onPointercancel(event);
    longPress.handlers.onPointercancel(event);
  }
  function onPointerleave(event: PointerEvent) {
    swipe.handlers.onPointerleave(event);
    longPress.handlers.onPointerleave(event);
  }

  function onContextMenu(event: MouseEvent) {
    // Suppress iOS Safari / Android native long-press menu while the gesture is
    // being handled. Desktop right-click (pointerType 'mouse' never sets
    // isPressing) remains untouched.
    if (longPress.isPressing.value) event.preventDefault();
  }

  /**
   * Whether the press behind the current click was a single-finger touch
   * that neither moved, scrolled nor long-pressed. One-shot: reading it
   * clears it, so a later click without a fresh press never reuses it.
   */
  function consumeStationaryTap(): boolean {
    const tap = lastPressWasTap;
    lastPressWasTap = false;
    return tap;
  }

  return {
    swipe,
    consumeStationaryTap,
    handlers: {
      onPointerdown,
      onPointermove,
      onPointerup,
      onPointercancel,
      onPointerleave,
      onContextMenu,
    },
  };
}
