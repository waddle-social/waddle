import { useHorizontalSwipe } from "@/ui/gestures/horizontal-swipe";
import { useLongPress } from "@/ui/gestures/long-press";

/**
 * Touch gestures on a message card: long-press opens the action sheet,
 * horizontal swipes trigger thread / reply. Both gesture recognizers see
 * every pointer event so a drag that starts as a potential long-press can
 * still resolve as a swipe (and vice versa cancel each other).
 */
export function useMessageGestures(input: {
  onLongPress: () => void;
  /** Right-to-left drag opens (or enters) the thread for this message. */
  onSwipeLeft: () => void;
  /** Left-to-right drag fills the composer reply chip targeting this
   * message — same path as the toolbar reply button. */
  onSwipeRight: () => void;
}) {
  const longPress = useLongPress({
    onLongPress: input.onLongPress,
  });

  const swipe = useHorizontalSwipe({
    onSwipeLeft: input.onSwipeLeft,
    onSwipeRight: input.onSwipeRight,
  });

  function onPointerdown(event: PointerEvent) {
    swipe.handlers.onPointerdown(event);
    longPress.handlers.onPointerdown(event);
  }
  function onPointermove(event: PointerEvent) {
    swipe.handlers.onPointermove(event);
    longPress.handlers.onPointermove(event);
  }
  function onPointerup(event: PointerEvent) {
    swipe.handlers.onPointerup(event);
    longPress.handlers.onPointerup(event);
  }
  function onPointercancel(event: PointerEvent) {
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

  return {
    swipe,
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
