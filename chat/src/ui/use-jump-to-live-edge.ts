import { ref, type ComputedRef, type Ref } from "vue";
import type { ScrollDirectionMode } from "@/lib/scroll-direction";

/**
 * Tracks whether a message scroll container has been scrolled away from
 * its "live edge" (the position where the newest message sits, which
 * depends on the user's scroll-direction preference):
 *
 *   - "social" / top-pinned mode → live edge is the TOP of the container.
 *     The user is away when scrollTop drifts past the threshold.
 *   - "chat" / bottom-pinned mode → live edge is the BOTTOM.
 *     The user is away when (scrollHeight − scrollTop − clientHeight)
 *     exceeds the threshold.
 *
 * When that happens the host component is expected to render a floating
 * "jump to latest" button. Click invokes `jump()` which delegates back
 * to the virtual-timeline-provided `scrollToEdge` callback.
 *
 * The composable doesn't own the scroll listener — the host wires its
 * existing `@scroll` handler to also call `updateDistance`. This keeps
 * the single-scroll-pass invariant the timeline already relies on.
 */
interface JumpToLiveEdgeHandle {
  /** Reactive: true when the user is "far enough" from the live edge. */
  shouldShow: Ref<boolean>;
  /** Call from the scroll handler to recompute on each scroll tick. */
  updateDistance: () => void;
  /** Click handler — scrolls back to the live edge. */
  jump: () => Promise<void>;
}

interface UseJumpToLiveEdgeInput {
  /** The scrolling element (the same one tracked by VirtualTimeline). */
  scrollElement: Ref<HTMLElement | null> | ComputedRef<HTMLElement | null>;
  /** Reactive scroll direction so the live-edge anchor flips when the
   * user toggles their preference mid-conversation. */
  mode: Ref<ScrollDirectionMode> | ComputedRef<ScrollDirectionMode>;
  /** Imperative scroll-to-edge handler — called by `jump()`. */
  scrollToEdge: (mode: ScrollDirectionMode) => Promise<boolean> | boolean;
  /** Pixels of drift from the live edge before the button appears.
   * Defaults to ~1 screen so casual scrolling for context doesn't
   * surface the button on every wheel tick. */
  threshold?: number;
}

export function useJumpToLiveEdge(input: UseJumpToLiveEdgeInput): JumpToLiveEdgeHandle {
  const threshold = input.threshold ?? 320;
  const shouldShow = ref(false);

  function updateDistance() {
    const el = input.scrollElement.value;
    if (!el) {
      shouldShow.value = false;
      return;
    }
    if (input.mode.value === "social") {
      shouldShow.value = el.scrollTop > threshold;
    } else {
      const bottomGap = el.scrollHeight - el.scrollTop - el.clientHeight;
      shouldShow.value = bottomGap > threshold;
    }
  }

  async function jump() {
    const result = input.scrollToEdge(input.mode.value);
    if (result instanceof Promise) await result;
    shouldShow.value = false;
  }

  return { shouldShow, updateDistance, jump };
}
