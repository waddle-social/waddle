import type { ComputedRef } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ExtensionAnnotationAction, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";

interface SendOrchestrationDeps {
  ui: ChatShellState;
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  isActiveDirectDmSurface: () => boolean;
  activeTarget: ComputedRef<ReturnType<typeof useChannelMessages> | ReturnType<typeof useDirectMessages>>;
  activeDmPeer: ComputedRef<{ peerJid: string } | null>;
}

/**
 * Message actions routed to whichever conversation surface is active:
 * send (main / thread / call-chat / GIF), edit, retract, react, displayed
 * markers, composing notifications, search, paging, and extension actions.
 */
export function useSendOrchestration(deps: SendOrchestrationDeps) {
  const { ui, xmppClient, waddles, messaging, dmMessaging, isActiveDirectDmSurface, activeTarget, activeDmPeer } = deps;

  async function sendActiveMessage(
    body?: string,
    markup?: MarkupSpan[],
    references?: MessageReference[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
    forumTitle?: string,
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) {
    if (isActiveDirectDmSurface()) {
      await dmMessaging.sendMessage(body, markup, references, files, replyTo, linkPreview);
      return;
    }
    await messaging.sendMessage(body, markup, references, files, replyTo, forumTitle, linkPreview);
  }

  async function sendPublicChannelMessage(body: string) {
    if (ui.sidebarMode.value !== "channels" || !xmppClient.value || !waddles.activeChannelId.value) {
      throw new Error("Public AI prompts require an active channel.");
    }
    ui.clearActionError();
    await messaging.sendMessage(body);
    if (ui.actionError.value) {
      throw new Error(ui.actionError.value);
    }
  }

  async function sendThreadMessage(
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) {
    if (isActiveDirectDmSurface()) {
      await dmMessaging.sendMessage(body, markup, references, files, replyTo, linkPreview, threadOverride);
      return;
    }
    await messaging.sendMessage(body, markup, references, files, replyTo, threadOverride, linkPreview);
  }

  async function sendCallChatMessage(
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) {
    ui.clearActionError();
    await sendThreadMessage(body, markup, references, files, replyTo, threadOverride, linkPreview);
    if (ui.actionError.value) {
      throw new Error(ui.actionError.value);
    }
  }

  function sendGif(
    url: string,
    threadOverride?: { threadId: string; parentThreadId?: string },
  ) {
    if (threadOverride) {
      void sendThreadMessage(url, [], [], undefined, undefined, threadOverride);
      return;
    }
    void sendActiveMessage(url, [], []);
  }

  function notifyActiveComposing(
    threadOverride?: { threadId: string; parentThreadId?: string },
  ) {
    // XEP-0201 §3: when typing originates in a thread composer, the
    // outbound XEP-0085 stanza echoes the active thread so peers can
    // scope the indicator instead of treating it as channel-wide.
    const thread = threadOverride
      ? threadOverride.parentThreadId
        ? { id: threadOverride.threadId, parent: threadOverride.parentThreadId }
        : { id: threadOverride.threadId }
      : undefined;
    activeTarget.value.notifyComposing(thread);
  }

  function editActiveMessage(messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[], linkPreview?: ComposerLinkPreviewSendPayload) {
    if (isActiveDirectDmSurface()) {
      void dmMessaging.editMessage(messageId, newBody, markup, references, linkPreview);
      return;
    }
    void messaging.editMessage(messageId, newBody, markup, references, linkPreview);
  }

  function retractActiveMessage(messageId: string) {
    void activeTarget.value.retractMessage(messageId);
  }

  function reactActiveMessage(messageId: string, emoji: string) {
    void activeTarget.value.toggleReaction(messageId, emoji);
  }

  function markActiveDisplayed(messageId: string, options?: { syncMds?: boolean }) {
    activeTarget.value.markDisplayed(messageId, options);
  }

  async function invokeActiveExtensionAction(action: ExtensionAnnotationAction) {
    return await activeTarget.value.invokeExtensionAction(action);
  }

  async function invokeExtensionRouteAction(action: ExtensionAnnotationAction) {
    await invokeActiveExtensionAction(action);
  }

  function searchActiveMessages(query: string) {
    void activeTarget.value.searchMessages(query);
  }

  function clearActiveSearch() {
    activeTarget.value.clearSearch();
  }

  function loadOlderActiveMessages() {
    void activeTarget.value.loadOlderMessages();
  }

  function retryActiveLoad() {
    const peer = activeDmPeer.value;
    if (!isActiveDirectDmSurface() || !peer) return;
    const peerJid = peer.peerJid;
    void (async () => {
      await xmppClient.value?.recoverSupersededSession();
      if (!isActiveDirectDmSurface() || activeDmPeer.value?.peerJid !== peerJid) return;
      await dmMessaging.loadMessages(peerJid);
    })();
  }

  function ensureActiveMessageLoaded(messageId: string) {
    return activeTarget.value.ensureMessageLoaded(messageId);
  }

  function loadOlderThreadMessages(threadId: string) {
    if (isActiveDirectDmSurface()) {
      void dmMessaging.loadOlderThreadMessages(threadId);
      return;
    }
    void messaging.loadOlderThreadMessages(threadId);
  }

  return {
    sendActiveMessage,
    sendPublicChannelMessage,
    sendThreadMessage,
    sendCallChatMessage,
    sendGif,
    notifyActiveComposing,
    editActiveMessage,
    retractActiveMessage,
    reactActiveMessage,
    markActiveDisplayed,
    invokeActiveExtensionAction,
    invokeExtensionRouteAction,
    searchActiveMessages,
    clearActiveSearch,
    loadOlderActiveMessages,
    retryActiveLoad,
    ensureActiveMessageLoaded,
    loadOlderThreadMessages,
  };
}
