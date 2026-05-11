import { ref, type Ref } from "vue";
import type { ChannelSummary } from "@/lib/chat-types";
import { isForumChannel } from "@/lib/channel-types";
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
import { applyForumContext } from "@/channels/timeline";
import { applyDeliveryEventById } from "@/lib/timeline-state";

type UseMucSendDeps = {
  session: Ref<WaddleSession | null>;
  xmppClient: Ref<BrowserXmppClient | null>;
  activeSpaceId: Ref<string | null>;
  activeChannelId: Ref<string | null>;
  currentChannel: Ref<ChannelSummary | null>;
  currentRoomJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
  draft: Ref<string>;
  forumPostTitle: Ref<string>;
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
  mentionJidsByNick?: Ref<Record<string, string>>;
  scrollToPinnedEdgeAndPin: () => Promise<boolean>;
  // Side-effect hook the orchestrator runs after a successful send — used
  // to clear chat-state debounce timers and mark XEP-0085 state as "active"
  // again. Keeps PR 2 free of chat-states ownership (extracted in PR 5).
  onSendComplete: (
    result: { state?: string } | null,
    spaceId: string,
    channelId: string,
    isStillCurrentChannel: boolean,
  ) => void;
};

export function useMucSend(deps: UseMucSendDeps) {
  const {
    session,
    xmppClient,
    activeSpaceId,
    activeChannelId,
    currentChannel,
    currentRoomJid,
    messages,
    draft,
    forumPostTitle,
    actionError,
    clearActionError,
    normalizeError,
    mentionJidsByNick,
    scrollToPinnedEdgeAndPin,
    onSendComplete,
  } = deps;

  const isSending = ref(false);
  const uploadProgress = ref({ uploading: false, progress: 0, filename: "" });

  // Client-assigned stanza ids still awaiting MUC-echo reconciliation (XEP-0045
  // §8.5 + XEP-0359 stable-id reflection). The live-merge composable (PR 4)
  // reads this set to body-match an unrecognised self-echo against pending
  // sends, so two identical bodies in flight don't retarget each other.
  const pendingEchoClientIds = new Set<string>();

  async function sendMessage(
    body?: string,
    markup?: MarkupSpan[],
    references?: MessageReference[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
    forumTitleOrThreadOverride?: string | { threadId: string; parentThreadId?: string },
  ) {
    const bodyText = body ?? draft.value;
    // markup !== undefined means this came from the rich editor (composer send)
    // undefined means it came from a programmatic send (GIF, etc.)
    const fromComposer = markup !== undefined;
    const threadOverride = typeof forumTitleOrThreadOverride === "string"
      ? undefined
      : forumTitleOrThreadOverride;
    const forumTitle = typeof forumTitleOrThreadOverride === "string"
      ? forumTitleOrThreadOverride
      : undefined;
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    const channelIsForum = isForumChannel(currentChannel.value);
    const hasFiles = !!files && files.length > 0;
    const isForumPost = channelIsForum && !replyTo;
    const resolvedForumTitle = (forumTitle ?? forumPostTitle.value).trim();
    const hasThreadMetadataIntent = !!threadOverride?.threadId.trim();
    const hasForumMetadataIntent = (isForumPost && !!resolvedForumTitle) || (channelIsForum && !!replyTo);
    if (!client || !channelId) return;
    if (!bodyText.trim() && !hasFiles && !hasThreadMetadataIntent && !hasForumMetadataIntent) return;
    if (isForumPost && !resolvedForumTitle) {
      actionError.value = "Add a title before posting to this forum.";
      return;
    }

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
      // XEP-0461 §3.2: groupchat replies MUST quote the room-assigned
      // XEP-0359 stanza-id. Without one, "messages without one cannot be
      // replied to"; refuse the send rather than leak a non-conformant id.
      if (replyTo && parent && !parent.replyableId) {
        actionError.value =
          "This message can't be replied to (no room stanza-id). Try reloading the channel.";
        isSending.value = false;
        return;
      }
      const wireReplyTo = replyTo && parent && parent.replyableId
        ? {
            id: parent.replyableId,
            author: parent.authorOccupantJid ?? parent.authorJid ?? replyTo.author,
            ...(replyTo.body ? { body: replyTo.body } : {}),
          }
        : undefined;
      // Thread membership is explicit via threadOverride, except forum replies,
      // which derive their thread from the replied-to topic/message.
      const threadId = threadOverride?.threadId
        ?? (channelIsForum && parent ? (parent.threadId ?? parent.id) : undefined);
      const parentThreadId = threadOverride?.parentThreadId;
      const threadCreate = isForumPost ? { title: resolvedForumTitle } : undefined;
      const threadReply = channelIsForum && replyTo && threadId
        ? { threadId }
        : undefined;
      const result = await client.sendGroupMessage(spaceId, channelId, bodyText, {
        markup,
        references,
        files: attachments,
        ...(wireReplyTo ? { replyTo: wireReplyTo } : {}),
        ...(threadId ? { threadId } : {}),
        ...(parentThreadId ? { parentThreadId } : {}),
        ...(threadCreate ? { threadCreate } : {}),
        ...(threadReply ? { threadReply } : {}),
        ...(mentionJidsByNick ? { mentionJidsByNick: mentionJidsByNick.value } : {}),
      });
      const msgId = result?.id ?? null;
      const isStillCurrentChannel =
        xmppClient.value === client &&
        (activeSpaceId.value ?? "") === spaceId &&
        activeChannelId.value === channelId;

      if (msgId && session.value && isStillCurrentChannel) {
        // Optimistic insert: show message immediately with "sending" status
        const optimistic: TimelineMessage = {
          id: msgId,
          correctionTargetId: msgId,
          author: session.value.username,
          authorJid: `${currentRoomJid.value}/${session.value.username}`,
          body: bodyText || (attachments?.[0]?.url ?? ""),
          createdAt: new Date().toISOString(),
          isSelf: true,
          deliveryStatus: (result?.state ?? "sending") as DeliveryStatus,
          ...(markup && markup.length > 0 ? { markup } : {}),
          ...(references && references.length > 0 ? { references } : {}),
        };
        if (replyTo && parent && parent.replyableId) {
          // Mirror the wire reply id on the optimistic insert so the local
          // chip and the eventual MAM round-trip resolve to the same id.
          optimistic.replyTo = {
            id: parent.replyableId,
            author: replyTo.author,
            ...(replyTo.body ? { preview: replyTo.body } : {}),
          };
        }
        if (threadId) optimistic.threadId = threadId;
        if (parentThreadId) optimistic.parentThreadId = parentThreadId;
        if (threadCreate) {
          optimistic.threadId = msgId;
          optimistic.forumPostKind = "topic";
          optimistic.forumTitle = threadCreate.title;
          optimistic.forumThreadTitle = threadCreate.title;
        } else if (threadReply) {
          optimistic.forumPostKind = "reply";
          const threadTitle = parent?.forumTitle ?? parent?.forumThreadTitle;
          if (threadTitle) optimistic.forumThreadTitle = threadTitle;
        }
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
        pendingEchoClientIds.add(msgId);
        messages.value = applyForumContext([...messages.value, optimistic]);
        void scrollToPinnedEdgeAndPin();
      }

      // Clear draft on successful send (triggers ChatEditor clear via watcher)
      if (fromComposer && isStillCurrentChannel) {
        draft.value = "";
        if (threadCreate) forumPostTitle.value = "";
      }
      // Isolate the post-send side-effect callback so a misbehaving
      // orchestrator (chat-state cleanup, XEP-0085 "active" send) can't be
      // mistaken for a send failure and surface as actionError.
      try {
        onSendComplete(result ?? null, spaceId, channelId, isStillCurrentChannel);
      } catch (callbackError) {
        console.warn("useMucSend onSendComplete callback threw", callbackError);
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
    if (!xmppClient.value || !activeChannelId.value || !newBody.trim()) return;
    const message = findMessageById(messages.value, messageId);
    const targetId = message?.correctionTargetId ?? messageId;

    clearActionError();

    try {
      await xmppClient.value.sendCorrection(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        newBody,
        targetId,
        markup,
        references,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  // XEP-0184 receipt / XEP-0198 SM ack from the wasm client. Idempotent —
  // see lib/xmpp/delivery-lifecycle.ts for the no-downgrade invariant.
  function onMessageAck(messageId: string) {
    messages.value = applyDeliveryEventById(messages.value, messageId, "delivered");
  }

  function onMessageQueueStatus(messageId: string, status: "queued" | "sending") {
    messages.value = applyDeliveryEventById(messages.value, messageId, status);
  }

  // XEP-0198: the XMPP client gave up on the stanza (resume failed or no
  // resumable transport).
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
