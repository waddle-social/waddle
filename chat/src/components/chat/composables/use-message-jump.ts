import { nextTick, ref, type Ref } from "vue";
import { timelineRowKey } from "@/lib/timeline-row-key";
import { resolveRoomMessageTarget } from "@/lib/messaging/room-message-target";
import { findMessageElementByRowKey } from "@/lib/message-targeting";
import { findMessageById } from "@/lib/message-ids";
import { getReplyJumpNotice } from "@/lib/reply-ux";
import type { TimelineMessage } from "@/lib/chat-ui";

interface JumpTimelineHandle {
  scrollToMessageId: (messageId: string, align?: "start" | "center" | "end") => Promise<boolean>;
}

/**
 * "Jump to message" behavior for the timeline: ensure the target is
 * loaded, resolve stanza-id aliases to the primary timeline id, scroll
 * it into view, and run the highlight flash — with a transient notice
 * when the target can't be reached. Timers are tracked so repeated
 * jumps don't race each other.
 */
export function useMessageJump(input: {
  messages: () => TimelineMessage[];
  ensureMessageLoaded: () => ((messageId: string) => Promise<boolean>) | undefined;
  virtualTimeline: () => JumpTimelineHandle | null;
  messagesContainer: Ref<HTMLDivElement | null>;
}) {
  const replyJumpNotice = ref("");
  let replyJumpNoticeTimeout: ReturnType<typeof setTimeout> | null = null;

  function clearReplyJumpNotice() {
    if (replyJumpNoticeTimeout) {
      clearTimeout(replyJumpNoticeTimeout);
      replyJumpNoticeTimeout = null;
    }
    replyJumpNotice.value = "";
  }

  function showReplyJumpNotice(message: string) {
    clearReplyJumpNotice();
    replyJumpNotice.value = message;
    replyJumpNoticeTimeout = setTimeout(() => {
      replyJumpNotice.value = "";
      replyJumpNoticeTimeout = null;
    }, 2800);
  }

  // Tracks pending flash timers so repeated jumps don't race each other — a
  // stale fade/cleanup from the previous animation would otherwise remove the
  // classes in the middle of the new flash.
  let flashEl: HTMLElement | null = null;
  let flashFadeTimer: ReturnType<typeof setTimeout> | null = null;
  let flashCleanupTimer: ReturnType<typeof setTimeout> | null = null;

  function cancelPendingFlash() {
    if (flashFadeTimer !== null) {
      clearTimeout(flashFadeTimer);
      flashFadeTimer = null;
    }
    if (flashCleanupTimer !== null) {
      clearTimeout(flashCleanupTimer);
      flashCleanupTimer = null;
    }
    if (flashEl) {
      flashEl.classList.remove("message-jump-flash", "message-jump-flash-fade");
      flashEl = null;
    }
  }

  async function scrollToMessage(messageId: string) {
    const ensureMessageLoaded = input.ensureMessageLoaded();
    if (ensureMessageLoaded && !await ensureMessageLoaded(messageId)) {
      showReplyJumpNotice(getReplyJumpNotice(false));
      return;
    }
    // Resolve protocol references first, then use the local presentation key
    // for both virtual scrolling and DOM highlighting.
    const messages = input.messages();
    const message = messages.some((row) => !!row.authorOccupantJid)
      ? resolveRoomMessageTarget(messages, messageId).message
      : findMessageById(messages, messageId);
    if (!message) {
      showReplyJumpNotice(getReplyJumpNotice(false));
      return;
    }
    const resolvedId = timelineRowKey(message);
    if (await input.virtualTimeline()?.scrollToMessageId(resolvedId, "center")) {
      await nextTick();
    }
    const el = findMessageElementByRowKey(input.messagesContainer.value, resolvedId);
    const notice = getReplyJumpNotice(el instanceof HTMLElement);
    if (!notice && el instanceof HTMLElement) {
      clearReplyJumpNotice();
      cancelPendingFlash();
      el.scrollIntoView({ behavior: "smooth", block: "center" });
      // Force a reflow before re-adding so repeat clicks re-trigger the animation.
      void el.offsetWidth;
      el.classList.add("message-jump-flash");
      flashEl = el;
      flashFadeTimer = setTimeout(() => {
        flashFadeTimer = null;
        el.classList.add("message-jump-flash-fade");
      }, 200);
      flashCleanupTimer = setTimeout(() => {
        flashCleanupTimer = null;
        el.classList.remove("message-jump-flash", "message-jump-flash-fade");
        if (flashEl === el) flashEl = null;
      }, 2000);
      return;
    }

    showReplyJumpNotice(notice);
  }

  return {
    replyJumpNotice,
    clearReplyJumpNotice,
    showReplyJumpNotice,
    scrollToMessage,
  };
}
