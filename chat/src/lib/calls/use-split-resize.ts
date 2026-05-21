import { computed, onBeforeUnmount, ref, type ComputedRef, type Ref } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callSplitPositions,
  SPLIT_DEFAULT_PERCENT,
  setSplitPercent,
} from "./split-position";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

/**
 * Pointer-driven row-resize composable for the call/chat splitter.
 *
 * Owned by `SplitDragHandle.vue`. The handle invokes `beginDrag` on
 * `pointerdown` with the pointer event and the bounding rect of the
 * shared flex column that the call region + chat region live inside.
 * The composable then attaches window-level `pointermove` /
 * `pointerup` listeners until the gesture ends, computing the new
 * split percentage from the pointer's Y relative to the column.
 *
 * Listeners are attached to `window` (not the handle element) so the
 * drag survives the pointer leaving the 8px-tall handle — otherwise
 * a fast drag would orphan the gesture and re-render the cursor as
 * "row-resize" without actually resizing anything.
 */
export function useSplitResize(roomJid: Ref<string>): {
  /** Read-only signal for visual feedback (cursor / overlay styling). */
  isDragging: Ref<boolean>;
  /** Current persisted split percentage for the bound room. Updates
   *  reactively as the user drags or as a different tab writes to
   *  localStorage via the store subscription. */
  percent: ComputedRef<number>;
  /** Attach to the handle's `@pointerdown`. */
  beginDrag: (
    event: PointerEvent,
    getColumnRect: () => DOMRect | null,
  ) => void;
} {
  const isDragging = ref(false);
  const positions = useStore($callSplitPositions);

  const percent = computed(() => {
    const key = normalizeMucCallRoomJid(roomJid.value);
    if (!key) return SPLIT_DEFAULT_PERCENT;
    return positions.value[key] ?? SPLIT_DEFAULT_PERCENT;
  });

  let activePointerId: number | null = null;
  let activeRect: DOMRect | null = null;
  let releaseGlobalSelection: (() => void) | null = null;

  function endDrag(): void {
    if (activePointerId !== null) {
      try {
        window.removeEventListener("pointermove", onPointerMove);
        window.removeEventListener("pointerup", onPointerUp);
        window.removeEventListener("pointercancel", onPointerUp);
      } catch {
        // ignore
      }
    }
    activePointerId = null;
    activeRect = null;
    isDragging.value = false;
    if (releaseGlobalSelection) {
      releaseGlobalSelection();
      releaseGlobalSelection = null;
    }
  }

  function onPointerMove(event: PointerEvent): void {
    if (event.pointerId !== activePointerId || !activeRect) return;
    if (activeRect.height <= 0) return;
    const offset = event.clientY - activeRect.top;
    const raw = (offset / activeRect.height) * 100;
    setSplitPercent(roomJid.value, raw);
  }

  function onPointerUp(event: PointerEvent): void {
    if (event.pointerId !== activePointerId) return;
    endDrag();
  }

  function beginDrag(
    event: PointerEvent,
    getColumnRect: () => DOMRect | null,
  ): void {
    if (event.button !== 0) return;
    const rect = getColumnRect();
    if (!rect) return;
    event.preventDefault();
    activePointerId = event.pointerId;
    activeRect = rect;
    isDragging.value = true;
    // Suppress text-selection across the document while dragging —
    // otherwise a vertical sweep highlights chat content beneath.
    const previous = document.body.style.userSelect;
    document.body.style.userSelect = "none";
    releaseGlobalSelection = () => {
      document.body.style.userSelect = previous;
    };
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
  }

  onBeforeUnmount(() => {
    if (activePointerId !== null) endDrag();
  });

  return { isDragging, percent, beginDrag };
}
