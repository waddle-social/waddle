import { watch, type Ref } from "vue";

/**
 * XEP-0333 displayed-marker gate for the thread panel: emit "displayed" for
 * the newest remote reply only while the panel is actually being read —
 * composer visible, no target scroll in flight, window focused, and the
 * scroller pinned at the live edge — and only once per
 * `(chat, thread, message)` key.
 */
export function useThreadDisplayedTracking(input: {
  activeThreadId: () => string | null;
  latestRemoteMessageId: () => string | null;
  hideComposer: () => boolean;
  targetScrollPending: () => boolean;
  scrollContainer: Ref<HTMLElement | null>;
  isWindowFocused: () => boolean;
  isPinnedAtEdge: () => boolean;
  /** Stable identity of the hosting chat surface for the dedupe key. */
  chatKey: () => string;
  emitDisplayed: (messageId: string) => void;
}) {
  let lastDisplayedThreadKey: string | null = null;

  function markThreadDisplayedIfVisible() {
    const threadId = input.activeThreadId();
    const messageId = input.latestRemoteMessageId();
    if (!threadId || !messageId) return;
    if (input.hideComposer() || input.targetScrollPending()) return;
    if (!input.scrollContainer.value || !input.isWindowFocused() || !input.isPinnedAtEdge()) return;

    const key = `${input.chatKey()}\0${threadId}\0${messageId}`;
    if (key === lastDisplayedThreadKey) return;
    lastDisplayedThreadKey = key;
    input.emitDisplayed(messageId);
  }

  watch(
    [
      () => input.activeThreadId(),
      () => input.latestRemoteMessageId(),
      () => input.hideComposer(),
      () => input.targetScrollPending(),
      () => input.scrollContainer.value,
      () => input.isWindowFocused(),
      () => input.isPinnedAtEdge(),
    ],
    () => markThreadDisplayedIfVisible(),
    { flush: "post", immediate: true },
  );

  return { markThreadDisplayedIfVisible };
}
