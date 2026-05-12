import { ref, type Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { MAX_FILE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";
import type { OutboundFileAttachment } from "@/lib/xmpp";
import {
  inferredFileDisposition,
  type DeliveryStatus,
  type MarkupSpan,
  type MessageReference,
  type TimelineMessage,
} from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { applyDeliveryEventById } from "@/lib/timeline-state";

// Derived from the wasm client's return shape; see useMucSend for rationale.
type ChatSendResult = Awaited<ReturnType<BrowserXmppClient["sendDirectMessage"]>>;

type UseChatSendDeps = {
  session: Ref<WaddleSession | null>;
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
  draft: Ref<string>;
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
  scrollToPinnedEdgeAndPin: () => Promise<boolean>;
  // Mirror of useMucSend's contract: the orchestrator handles XEP-0085 chat
  // state cleanup until that axis lands its own composable in PR 5.
  onSendComplete: (
    result: ChatSendResult,
    peerJid: string,
    isStillActive: boolean,
  ) => void;
};

export function useChatSend(deps: UseChatSendDeps) {
  const {
    session,
    xmppClient,
    activePeerJid,
    messages,
    draft,
    actionError,
    clearActionError,
    normalizeError,
    scrollToPinnedEdgeAndPin,
    onSendComplete,
  } = deps;

  const isSending = ref(false);
  const uploadProgress = ref({ uploading: false, progress: 0, filename: "" });

  // Client-assigned stanza ids still awaiting reconciliation. DMs don't have
  // MUC self-echo, but a recently-queued message replayed from
  // outbound-queue-store needs the same body-match fallback when the carbon
  // arrives back from the server. Live-merge (PR 4) consumes this set.
  const pendingEchoClientIds = new Set<string>();

  async function sendMessage(
    explicitBody?: string,
    markup?: MarkupSpan[],
    references?: MessageReference[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
  ) {
    const bodyText = explicitBody ?? draft.value;
    const fromComposer = markup !== undefined;
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    const hasFiles = !!files && files.length > 0;
    if (!client || !peerJid || !session.value) return;
    if (!bodyText.trim() && !hasFiles) return;

    if (hasFiles) {
      for (const f of files!) {
        if (f.size > MAX_FILE_UPLOAD_BYTES) {
          actionError.value = `File too large (${(f.size / 1024 / 1024).toFixed(1)} MB). Maximum upload size is 10 MB.`;
          return;
        }
      }
    }

    isSending.value = true;
    clearActionError();
    try {
      let attachments: OutboundFileAttachment[] | undefined;
      if (hasFiles) {
        const filenames = files!.map((f) => (f instanceof File ? f.name : `attachment-${Date.now()}.bin`));
        uploadProgress.value = { uploading: true, progress: 0, filename: filenames[0] };
        attachments = await client.uploadAttachments(files!, (overall, idx) => {
          uploadProgress.value = {
            uploading: true,
            progress: overall.total > 0 ? overall.loaded / overall.total : 0,
            filename: filenames[idx] ?? "",
          };
        });
      }

      const parent = replyTo ? findMessageById(messages.value, replyTo.id) : undefined;
      const wireReplyTo = replyTo && parent
        ? {
            id: parent.id,
            author: parent.authorJid ?? replyTo.author,
            ...(replyTo.body ? { body: replyTo.body } : {}),
          }
        : undefined;
      const threadId = parent ? (parent.threadId ?? parent.id) : undefined;
      const result = await client.sendDirectMessage(peerJid, bodyText, {
        markup,
        references,
        files: attachments,
        ...(wireReplyTo ? { replyTo: wireReplyTo } : {}),
        ...(threadId ? { threadId } : {}),
      });
      const msgId = result?.id ?? null;
      const isStillActive = xmppClient.value === client && activePeerJid.value === peerJid;
      if (isStillActive) {
        if (msgId) {
          pendingEchoClientIds.add(msgId);
          const optimistic: TimelineMessage = {
            id: msgId,
            correctionTargetId: msgId,
            author: session.value.username,
            authorJid: session.value.jid,
            body: bodyText || (attachments?.[0]?.url ?? ""),
            createdAt: new Date().toISOString(),
            isSelf: true,
            deliveryStatus: (result?.state ?? "sending") as DeliveryStatus,
            ...(markup && markup.length > 0 ? { markup } : {}),
            ...(references && references.length > 0 ? { references } : {}),
          };
          if (replyTo && parent) {
            optimistic.replyTo = {
              id: parent.id,
              author: replyTo.author,
              ...(replyTo.body ? { preview: replyTo.body } : {}),
            };
          }
          if (threadId) optimistic.threadId = threadId;
          if (attachments && attachments.length > 0) {
            optimistic.sharedFiles = attachments.map((a) => ({
              url: a.url,
              name: a.name,
              mediaType: a.mediaType,
              size: a.size,
              ...(a.width ? { width: a.width } : {}),
              ...(a.height ? { height: a.height } : {}),
              disposition: inferredFileDisposition(a.mediaType, a.name ?? a.url),
              ...(a.encrypted ? { encrypted: a.encrypted } : {}),
            }));
          }
          messages.value = [...messages.value, optimistic];
          void scrollToPinnedEdgeAndPin();
        }
        if (fromComposer) draft.value = "";
      }
      // Isolate the post-send side-effect callback so a misbehaving
      // orchestrator (chat-state cleanup, XEP-0085 "active" send) can't be
      // mistaken for a send failure and surface as actionError.
      try {
        onSendComplete(result ?? null, peerJid, isStillActive);
      } catch (callbackError) {
        console.warn("useChatSend onSendComplete callback threw", callbackError);
      }
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSending.value = false;
      uploadProgress.value = { uploading: false, progress: 0, filename: "" };
    }
  }

  async function editMessage(
    messageId: string,
    newBody: string,
    markup?: MarkupSpan[],
    references?: MessageReference[],
  ) {
    if (!xmppClient.value || !activePeerJid.value || !newBody.trim()) return;
    const message = findMessageById(messages.value, messageId);
    const targetId = message?.correctionTargetId ?? messageId;
    clearActionError();
    try {
      await xmppClient.value.sendDmCorrection(activePeerJid.value, newBody, targetId, markup, references);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  function onMessageAck(messageId: string) {
    messages.value = applyDeliveryEventById(messages.value, messageId, "delivered");
  }

  function onMessageQueueStatus(messageId: string, status: "queued" | "sending") {
    messages.value = applyDeliveryEventById(messages.value, messageId, status);
  }

  function onMessageDeliveryFailure(messageId: string) {
    messages.value = applyDeliveryEventById(messages.value, messageId, "failed");
  }

  return {
    isSending,
    uploadProgress,
    pendingEchoClientIds,
    sendMessage,
    editMessage,
    onMessageAck,
    onMessageQueueStatus,
    onMessageDeliveryFailure,
  };
}
