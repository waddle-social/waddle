import { nextTick, watch, type Ref } from "vue";
import { captureOlderPageScroll } from "@/lib/pinned-edge-scroll";
import type { ScrollDirectionMode } from "@/lib/scroll-direction";

interface OlderLoadSnapshot {
  element: HTMLElement;
  mode: ScrollDirectionMode;
  threadId: string | null;
  token: number;
}

/**
 * Keeps the reader's place when older replies arrive. Record which thread
 * started loading, then read the current scroll position before Vue updates
 * the rows. Only bottom-pinned mode prepends above the viewport.
 */
export function useThreadOlderLoadRestore(input: {
  isLoadingOlderReplies: () => boolean | undefined;
  scrollContainer: Ref<HTMLElement | null>;
  mode: () => ScrollDirectionMode;
  activeThreadId: () => string | null;
  /** Loading starts: drop any settle lock so the restore wins. */
  cancelSettleLock: () => void;
  /** Runs after every restore attempt (re-probe the day marker). */
  afterRestore: () => void;
}) {
  let olderLoadSnapshot: OlderLoadSnapshot | null = null;
  let olderLoadRestoreToken = 0;

  watch(
    () => input.isLoadingOlderReplies(),
    (loading, wasLoading) => {
      if (loading) {
        input.cancelSettleLock();
        const el = input.scrollContainer.value;
        olderLoadRestoreToken++;
        olderLoadSnapshot = el
          ? {
              element: el,
              mode: input.mode(),
              threadId: input.activeThreadId(),
              token: olderLoadRestoreToken,
            }
          : null;
        return;
      }
      if (!wasLoading || !olderLoadSnapshot) return;
      const snapshot = olderLoadSnapshot;
      olderLoadSnapshot = null;
      const restore = captureOlderPageScroll(snapshot.element, snapshot.mode);
      void nextTick(() => {
        const el = input.scrollContainer.value;
        if (
          !input.isLoadingOlderReplies()
          && snapshot.token === olderLoadRestoreToken
          && el === snapshot.element
          && input.activeThreadId() === snapshot.threadId
        ) {
          restore?.(el, input.mode());
        }
        input.afterRestore();
      });
    },
  );
}
