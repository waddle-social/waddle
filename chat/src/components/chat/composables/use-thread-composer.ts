import { nextTick, ref } from "vue";
import type { MarkupSpan, MessageReference, TimelineMessage } from "@/lib/chat-ui";
import type { ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";

export interface ThreadSendOverride {
  threadId: string;
  parentThreadId?: string;
}

export interface ThreadReplyTarget {
  id: string;
  author: string;
  body?: string;
}

type ComposerHandle = { focus: () => void };

/**
 * Composer state for the thread panel: the draft, the in-thread reply
 * target, and the send/GIF/typing handlers that stamp every outbound
 * payload with the active thread override (XEP-0201 `<thread/>`, plus the
 * parent thread when nested) so it joins the open thread instead of the
 * parent channel timeline.
 */
export function useThreadComposer(input: {
  activeThreadId: () => string | null;
  parentThreadId: () => string | undefined;
  rootMessage: () => TimelineMessage | null;
  hideComposer: () => boolean;
  emitSend: (
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: ThreadReplyTarget | undefined,
    threadOverride: ThreadSendOverride,
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) => void;
  emitSelectGif: (url: string, threadOverride: ThreadSendOverride) => void;
  emitTyping: (threadOverride?: ThreadSendOverride) => void;
}) {
  const draft = ref("");
  const replyingTo = ref<ThreadReplyTarget | null>(null);
  const composerRef = ref<ComposerHandle | null>(null);

  function setComposerRef(el: ComposerHandle | null) {
    composerRef.value = el;
  }

  function activeThreadOverride(): ThreadSendOverride | null {
    const threadId = input.activeThreadId();
    if (!threadId) return null;
    const override: ThreadSendOverride = { threadId };
    const parentThreadId = input.parentThreadId();
    if (parentThreadId) override.parentThreadId = parentThreadId;
    return override;
  }

  function beginReplyInThread(message: TimelineMessage) {
    if (input.hideComposer()) return;
    replyingTo.value = {
      id: message.id,
      author: message.author,
      ...(message.body ? { body: message.body } : {}),
    };
    void nextTick(() => composerRef.value?.focus());
  }

  function cancelReplyInThread() {
    replyingTo.value = null;
  }

  // Switching threads discards the draft and any pending reply target.
  function resetForThreadSwitch() {
    replyingTo.value = null;
    draft.value = "";
  }

  function onSend(
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files?: Array<File | Blob>,
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) {
    const override = activeThreadOverride();
    if (!override) return;
    const root = input.rootMessage();
    const pending = replyingTo.value;
    // Default target = the thread root so the reply chip still points
    // somewhere useful; explicit in-thread reply overrides it.
    const effectiveReply = pending
      ? { id: pending.id, author: pending.author, ...(pending.body ? { body: pending.body } : {}) }
      : root
        ? { id: root.id, author: root.authorJid ?? root.author, ...(root.body ? { body: root.body } : {}) }
        : undefined;
    input.emitSend(body, markup, references, files, effectiveReply, override, linkPreview);
    replyingTo.value = null;
    draft.value = "";
  }

  // GIF picks mirror onSend's thread routing: derive the active thread (and
  // parent, when nested) so the GIF message joins the open thread instead of
  // landing in the parent channel timeline.
  function onSelectGif(url: string) {
    const override = activeThreadOverride();
    if (!override) return;
    input.emitSelectGif(url, override);
  }

  // Forward composer typing with the active thread context attached so the
  // outbound XEP-0085 chat-state stanza can include the matching `<thread/>`.
  function onTyping() {
    const override = activeThreadOverride();
    if (!override) {
      input.emitTyping();
      return;
    }
    input.emitTyping(override);
  }

  return {
    draft,
    replyingTo,
    setComposerRef,
    beginReplyInThread,
    cancelReplyInThread,
    resetForThreadSwitch,
    onSend,
    onSelectGif,
    onTyping,
  };
}
