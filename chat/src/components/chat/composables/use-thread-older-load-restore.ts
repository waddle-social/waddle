import { nextTick, watch, type Ref } from "vue";
import { isTopPinnedScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";

interface OlderLoadSnapshot {
  element: HTMLElement;
  height: number;
  mode: ScrollDirectionMode;
  threadId: string | null;
  top: number;
  token: number;
}

/**
 * Keeps the reader's place while older replies paginate in: snapshot the
 * scroll position when loading starts and, once it finishes, offset by the
 * grown scroll height — but only if the container, thread, and scroll mode
 * are still the ones we snapshotted (bottom-pinned mode only; top-pinned
 * prepends below the viewport so no correction is needed).
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
              height: el.scrollHeight,
              mode: input.mode(),
              threadId: input.activeThreadId(),
              top: el.scrollTop,
              token: olderLoadRestoreToken,
            }
          : null;
        return;
      }
      if (!wasLoading || !olderLoadSnapshot) return;
      const snapshot = olderLoadSnapshot;
      olderLoadSnapshot = null;
      void nextTick(() => {
        const el = input.scrollContainer.value;
        if (
          !input.isLoadingOlderReplies()
          && snapshot.token === olderLoadRestoreToken
          && el === snapshot.element
          && input.activeThreadId() === snapshot.threadId
          && input.mode() === snapshot.mode
          && !isTopPinnedScrollDirection(input.mode())
        ) {
          el.scrollTop = snapshot.top + (el.scrollHeight - snapshot.height);
        }
        input.afterRestore();
      });
    },
  );
}
