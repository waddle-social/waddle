import { computed, nextTick, ref, watch } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";

interface TargetScrollTimeline {
  scrollToMessageId: (messageId: string, align?: "start" | "center" | "end") => Promise<boolean>;
}

/**
 * "Open thread at message X" orchestration for the thread panel: keeps a
 * pending flag while the target message hasn't been rendered yet (older
 * pages still loading, timeline not mounted), scrolls it into view exactly
 * once per `(thread, message, request)` key, and reports completion so
 * displayed-markers can resume.
 */
export function useThreadTargetScroll(input: {
  targetMessageId: () => string | null | undefined;
  targetMessageRequestId: () => number;
  activeThreadId: () => string | null;
  orderedMessages: () => readonly TimelineMessage[];
  timeline: () => TargetScrollTimeline | null;
  /** Post-scroll bookkeeping: re-probe pinned state and the day marker. */
  afterScroll: () => void;
  /** Invoked when target scrolling goes idle so displayed-markers resume. */
  markDisplayed: () => void;
}) {
  const targetScrollPending = ref(Boolean(input.targetMessageId()));
  let targetScrollToken = 0;
  let completedTargetScrollKey: string | null = null;

  const orderedMessageIdsKey = computed(() =>
    input.orderedMessages().map((message) => message.id).join("\0"),
  );

  function targetScrollKey(): string | null {
    const threadId = input.activeThreadId();
    const targetMessageId = input.targetMessageId();
    return threadId && targetMessageId
      ? `${threadId}\0${targetMessageId}\0${input.targetMessageRequestId()}`
      : null;
  }

  /** Abandon any in-flight target scroll (e.g. on thread switch). */
  function cancelPendingTargetScroll() {
    targetScrollToken++;
    targetScrollPending.value = false;
    completedTargetScrollKey = null;
  }

  async function scrollToTargetMessage() {
    const token = ++targetScrollToken;
    const key = targetScrollKey();
    const targetMessageId = input.targetMessageId();
    if (!targetMessageId || !key) {
      targetScrollPending.value = false;
      completedTargetScrollKey = null;
      input.markDisplayed();
      return;
    }
    if (completedTargetScrollKey === key) {
      targetScrollPending.value = false;
      return;
    }
    if (
      !input.activeThreadId()
      || !input.orderedMessages().some((message) => message.id === targetMessageId)
    ) {
      targetScrollPending.value = true;
      return;
    }
    const timeline = input.timeline();
    if (!timeline) {
      targetScrollPending.value = true;
      return;
    }
    targetScrollPending.value = true;
    await nextTick();
    if (!await timeline.scrollToMessageId(targetMessageId, "center")) {
      if (token === targetScrollToken) targetScrollPending.value = false;
      return;
    }
    await nextTick();
    if (token !== targetScrollToken) return;
    input.afterScroll();
    completedTargetScrollKey = key;
    targetScrollPending.value = false;
    input.markDisplayed();
  }

  watch(
    () => input.targetMessageId(),
    () => {
      completedTargetScrollKey = null;
    },
  );

  watch(
    [
      () => input.targetMessageId(),
      () => input.targetMessageRequestId(),
      () => input.activeThreadId(),
      orderedMessageIdsKey,
    ],
    () => {
      void scrollToTargetMessage();
    },
    { flush: "post" },
  );

  return {
    targetScrollPending,
    scrollToTargetMessage,
    cancelPendingTargetScroll,
  };
}
