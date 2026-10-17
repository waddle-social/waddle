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
 * Also remembers whether the latest press was a stationary touch, so the
 * row's click handler can tell a plain tap from the click a browser
 * synthesises after a swipe, scroll or long press.
 */
export function useMessageGestures(input: {
  onLongPress: () => void;
  /** Right-to-left drag opens (or enters) the thread for this message. */
  onSwipeLeft: () => void;
  /** Left-to-right drag fills the composer reply chip targeting this
   * message — same path as the toolbar reply button. */
  onSwipeRight: () => void;
}) {
  let tapPointerId: number | null = null;
  let tapIsTouch = false;
  let tapDisqualified = false;
  let tapStartX = 0;
  let tapStartY = 0;

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
    tapPointerId = event.pointerId;
    tapIsTouch = event.pointerType === "touch";
    tapDisqualified = false;
    tapStartX = event.clientX;
    tapStartY = event.clientY;
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
    swipe.handlers.onPointerup(event);
    longPress.handlers.onPointerup(event);
  }
  function onPointercancel(event: PointerEvent) {
    // The browser took the pointer (scroll, pinch): whatever follows is not a tap.
    if (event.pointerId === tapPointerId) tapDisqualified = true;
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

  /** True when the latest press was a touch that neither moved, scrolled nor long-pressed. */
  function lastPressWasStationaryTouch(): boolean {
    return tapPointerId !== null && tapIsTouch && !tapDisqualified;
  }

  return {
    swipe,
    lastPressWasStationaryTouch,
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
