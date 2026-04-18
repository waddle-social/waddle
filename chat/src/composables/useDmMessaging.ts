import { nextTick, ref, type Ref } from "vue";
import type { DeliveryStatus, TimelineMessage } from "@/lib/chat-ui";
import type {
  BrowserXmppClient,
  ChatStateType,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  SessionLifecycleEvent,
} from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { MAX_IMAGE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";
import type { OutboundFileAttachment } from "@/lib/xmpp";

function fromLiveDmMessage(session: WaddleSession, msg: LiveDmMessage): TimelineMessage {
  const tm: TimelineMessage = {
    id: msg.id,
    author: msg.nick,
    authorJid: msg.fromJid,
    body: msg.body,
    createdAt: msg.createdAt,
    isSelf: barePeerJid(msg.fromJid) === barePeerJid(session.jid),
  };
  if (msg.mentions?.length) tm.mentions = msg.mentions;
  if (msg.markup?.length) tm.markup = msg.markup;
  if (msg.sharedFiles && msg.sharedFiles.length > 0) tm.sharedFiles = msg.sharedFiles;
  if (msg.isSticker) tm.isSticker = true;
  if (msg.replyTo) {
    tm.replyTo = { id: msg.replyTo.id, ...(msg.replyTo.author ? { author: msg.replyTo.author } : {}) };
  }
  if (msg.threadId) tm.threadId = msg.threadId;
  if (msg.parentThreadId) tm.parentThreadId = msg.parentThreadId;
  return tm;
}

export function useDmMessaging(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activePeerJid: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const peerNameFromJid = (jid: string) => barePeerJid(jid).split("@")[0] ?? "unknown";
  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const isLoadingMessages = ref(false);
  const isSending = ref(false);
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const typingUsers = ref<string[]>([]);
  const searchResults = ref<{ id: string; nick: string; body: string; createdAt: string }[]>([]);
  const isSearching = ref(false);
  const uploadProgress = ref({ uploading: false, progress: 0, filename: "" });

  let messageRequestId = 0;
  let searchRequestId = 0;
  let lastChatState: ChatStateType = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  // Client-assigned stanza ids still awaiting server-echo reconciliation.
  // Only these participate in the body-based fallback match so repeated
  // identical text doesn't mis-target already-reconciled messages.
  const pendingEchoClientIds = new Set<string>();

  async function scrollToBottom() {
    await nextTick();
    await nextTick();
    if (timelineEl.value) {
      timelineEl.value.scrollTop = timelineEl.value.scrollHeight;
    }
  }

  function addTypingUser(nick: string) {
    if (!typingUsers.value.includes(nick)) {
      typingUsers.value = [...typingUsers.value, nick];
    }
    const existing = typingTimers.get(nick);
    if (existing) clearTimeout(existing);
    typingTimers.set(nick, setTimeout(() => removeTypingUser(nick), 5000));
  }

  function removeTypingUser(nick: string) {
    const timer = typingTimers.get(nick);
    if (timer) {
      clearTimeout(timer);
      typingTimers.delete(nick);
    }
    if (typingUsers.value.includes(nick)) {
      typingUsers.value = typingUsers.value.filter((n) => n !== nick);
    }
  }

  function clearTypingState() {
    for (const timer of typingTimers.values()) clearTimeout(timer);
    typingTimers.clear();
    typingUsers.value = [];
    lastChatState = "active";
    if (composingTimeout) {
      clearTimeout(composingTimeout);
      composingTimeout = null;
    }
  }

  function applyDisplayed(messageId: string, nick: string) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.id !== messageId) return m;
      const existing = m.readBy ? [...m.readBy] : [];
      if (!existing.includes(nick)) existing.push(nick);
      return { ...m, readBy: existing };
    });
  }

  function applyReaction(messageId: string, nick: string, emojis: string[]) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.id !== messageId) return m;
      const existing: Record<string, string[]> = m.reactions ? { ...m.reactions } : {};
      for (const key of Object.keys(existing)) {
        existing[key] = (existing[key] ?? []).filter((n) => n !== nick);
        if (existing[key].length === 0) delete existing[key];
      }
      for (const emoji of emojis) {
        if (!existing[emoji]) existing[emoji] = [];
        existing[emoji].push(nick);
      }
      const updated = { ...m };
      if (Object.keys(existing).length > 0) updated.reactions = existing;
      else delete updated.reactions;
      return updated;
    });
  }

  function applyRetraction(retractsId: string) {
    messages.value = messages.value.map((m) => (m.id === retractsId ? { ...m, body: "", isRetracted: true } : m));
  }

  function applyCorrection(replacesId: string, newBody: string, markup?: LiveDmMessage["markup"]) {
    messages.value = messages.value.map((m) => {
      if (m.id !== replacesId) return m;
      const updated: TimelineMessage = { ...m, body: newBody.trim(), isEdited: true };
      if (markup && markup.length > 0) {
        updated.markup = markup;
      } else {
        delete updated.markup;
      }
      return updated;
    });
  }

  function mergeLiveMessage(msg: TimelineMessage) {
    // Self-echo reconciliation: match by id first; otherwise body-match only
    // against messages still awaiting reconciliation so duplicates don't
    // retarget already-reconciled entries. Echo = authoritative → delivered.
    const existing = messages.value.find(
      (m) =>
        m.id === msg.id ||
        (pendingEchoClientIds.has(m.id) && m.isSelf && msg.isSelf && m.body === msg.body),
    );
    if (existing) {
      const wasPending = existing.id !== msg.id;
      if (wasPending || existing.deliveryStatus === "sending") {
        messages.value = messages.value.map((m) =>
          m.id !== existing.id
            ? m
            : { ...m, id: msg.id, deliveryStatus: "delivered" as DeliveryStatus },
        );
      }
      if (wasPending) pendingEchoClientIds.delete(existing.id);
      return;
    }
    messages.value = [...messages.value, msg];
    void scrollToBottom();
  }

  /** XEP-0198: SM ack promotes the matching self-sent message to delivered. */
  function onMessageAck(messageId: string) {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus === "sending"
        ? { ...m, deliveryStatus: "delivered" as DeliveryStatus }
        : m,
    );
  }

  /** XEP-0198: stanza.js gave up on the stanza — surface as failed so the
   *  user can retry. */
  function onMessageDeliveryFailure(messageId: string) {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus !== "delivered"
        ? { ...m, deliveryStatus: "failed" as DeliveryStatus }
        : m,
    );
  }

  /** On a fresh session (SM resume failed), re-fetch MAM to close any gap
   *  for the currently-open conversation. Local optimistic sends are
   *  preserved across the reload so the user keeps retry affordances. */
  function onSessionLifecycle(event: SessionLifecycleEvent) {
    if (event.type !== "fresh") return;
    const peerJid = activePeerJid.value;
    if (!peerJid) return;
    if (messages.value.length === 0) return;
    const preserved = messages.value.filter(
      (m) =>
        m.isSelf && (m.deliveryStatus === "sending" || m.deliveryStatus === "failed"),
    );
    void (async () => {
      await loadMessages(peerJid);
      if (preserved.length === 0 || activePeerJid.value !== peerJid) return;
      const existingIds = new Set(messages.value.map((m) => m.id));
      const toAppend = preserved.filter((m) => !existingIds.has(m.id));
      if (toAppend.length > 0) messages.value = [...messages.value, ...toAppend];
    })();
  }

  async function loadMessages(peerJid: string) {
    if (!session.value) return;
    const requestId = ++messageRequestId;
    isLoadingMessages.value = true;
    clearActionError();
    try {
      const mamResults = xmppClient.value ? await xmppClient.value.queryPersonalMam(peerJid, 100) : [];
      if (requestId !== messageRequestId || activePeerJid.value !== peerJid) return;
      const regular: LiveDmMessage[] = [];
      const reactionUpdates: { targetId: string; nick: string; emojis: string[] }[] = [];
      const retractionUpdates: string[] = [];
      const correctionUpdates: { targetId: string; body: string; markup?: LiveDmMessage["markup"] }[] = [];
      for (const msg of mamResults) {
        if (msg._reactionTarget && msg._reactionEmojis) {
          reactionUpdates.push({
            targetId: msg._reactionTarget,
            nick: msg.nick,
            emojis: msg._reactionEmojis,
          });
        } else if (msg.retractsId) {
          retractionUpdates.push(msg.retractsId);
        } else if (msg.replacesId) {
          correctionUpdates.push({
            targetId: msg.replacesId,
            body: msg.body,
            markup: msg.markup,
          });
        } else if (msg.body || (msg.sharedFiles && msg.sharedFiles.length > 0) || msg.isSticker) {
          regular.push(msg);
        }
      }
      const timeline = regular.map((m) => fromLiveDmMessage(session.value!, m));
      for (const update of correctionUpdates) {
        const target = timeline.find((m) => m.id === update.targetId);
        if (!target) continue;
        target.body = update.body.trim();
        target.isEdited = true;
        if (update.markup && update.markup.length > 0) {
          target.markup = update.markup;
        } else {
          delete target.markup;
        }
      }
      for (const retractsId of retractionUpdates) {
        const target = timeline.find((m) => m.id === retractsId);
        if (!target) continue;
        target.body = "";
        target.isRetracted = true;
      }
      for (const update of reactionUpdates) {
        const target = timeline.find((m) => m.id === update.targetId);
        if (!target) continue;
        const reactions: Record<string, string[]> = target.reactions ? { ...target.reactions } : {};
        for (const emoji of update.emojis) {
          if (!reactions[emoji]) reactions[emoji] = [];
          if (!reactions[emoji].includes(update.nick)) reactions[emoji].push(update.nick);
        }
        target.reactions = reactions;
      }
      messages.value = timeline;
      if (requestId === messageRequestId) isLoadingMessages.value = false;
      await scrollToBottom();
    } catch (e) {
      if (requestId === messageRequestId) {
        actionError.value = normalizeError(e);
        isLoadingMessages.value = false;
      }
    }
  }

  async function sendMessage(
    explicitBody?: string,
    _markup?: unknown,
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
  ) {
    const bodyText = explicitBody ?? draft.value;
    const fromComposer = _markup !== undefined;
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    const hasFiles = !!files && files.length > 0;
    if (!client || !peerJid || !session.value) return;
    if (!bodyText.trim() && !hasFiles) return;

    if (hasFiles) {
      for (const f of files!) {
        if (f.size > MAX_IMAGE_UPLOAD_BYTES) {
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
        const filenames = files!.map((f) => (f instanceof File ? f.name : `image-${Date.now()}.png`));
        uploadProgress.value = { uploading: true, progress: 0, filename: filenames[0] };
        attachments = await client.uploadAttachments(files!, (overall, idx) => {
          uploadProgress.value = {
            uploading: true,
            progress: overall.total > 0 ? overall.loaded / overall.total : 0,
            filename: filenames[idx] ?? "",
          };
        });
      }

      const parent = replyTo ? messages.value.find((m) => m.id === replyTo.id) : undefined;
      const wireReplyTo = replyTo && parent
        ? {
            id: replyTo.id,
            author: parent.authorJid ?? replyTo.author,
            ...(replyTo.body ? { body: replyTo.body } : {}),
          }
        : undefined;
      const threadId = parent ? (parent.threadId ?? replyTo?.id) : undefined;
      const msgId = await client.sendDirectMessage(peerJid, bodyText, {
        files: attachments,
        ...(wireReplyTo ? { replyTo: wireReplyTo } : {}),
        ...(threadId ? { threadId } : {}),
      });
      const isStillActive = xmppClient.value === client && activePeerJid.value === peerJid;
      if (isStillActive) {
        if (msgId) {
          pendingEchoClientIds.add(msgId);
          const optimistic: TimelineMessage = {
            id: msgId,
            author: session.value.username,
            authorJid: session.value.jid,
            body: bodyText.trim() || (attachments?.[0]?.url ?? ""),
            createdAt: new Date().toISOString(),
            isSelf: true,
            deliveryStatus: "sending",
          };
          if (replyTo && parent) {
            optimistic.replyTo = {
              id: replyTo.id,
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
              disposition: "inline" as const,
            }));
          }
          messages.value = [...messages.value, optimistic];
          void scrollToBottom();
        }
        if (fromComposer) draft.value = "";
        if (composingTimeout) {
          clearTimeout(composingTimeout);
          composingTimeout = null;
        }
        lastChatState = "active";
      }
      void client.sendDmChatState(peerJid, "active").catch(() => undefined);
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSending.value = false;
      uploadProgress.value = { uploading: false, progress: 0, filename: "" };
    }
  }

  async function toggleReaction(messageId: string, emoji: string) {
    if (!xmppClient.value || !activePeerJid.value || !session.value) return;
    const msg = messages.value.find((m) => m.id === messageId);
    const myNick = session.value.username;
    const currentReactions = msg?.reactions ?? {};
    const myEmojis = new Set<string>();
    for (const [e, nicks] of Object.entries(currentReactions)) {
      if (nicks.includes(myNick)) myEmojis.add(e);
    }
    if (myEmojis.has(emoji)) myEmojis.delete(emoji);
    else myEmojis.add(emoji);
    try {
      await xmppClient.value.sendDmReaction(activePeerJid.value, messageId, [...myEmojis]);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activePeerJid.value) return;
    clearActionError();
    try {
      await xmppClient.value.sendDmRetraction(activePeerJid.value, messageId);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function editMessage(messageId: string, newBody: string) {
    if (!xmppClient.value || !activePeerJid.value || !newBody.trim()) return;
    clearActionError();
    try {
      await xmppClient.value.sendDmCorrection(activePeerJid.value, newBody, messageId);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activePeerJid.value) return;
    xmppClient.value.sendDmDisplayed(activePeerJid.value, messageId);
  }

  function notifyComposing() {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid) return;
    if (lastChatState !== "composing") {
      lastChatState = "composing";
      client.sendDmChatState(peerJid, "composing");
    }
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (xmppClient.value !== client || activePeerJid.value !== peerJid) return;
      lastChatState = "paused";
      client.sendDmChatState(peerJid, "paused");
    }, 3000);
  }

  async function searchMessages(query: string) {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid) return;
    const requestId = ++searchRequestId;
    const trimmed = query.trim();
    if (!trimmed) {
      searchResults.value = [];
      isSearching.value = false;
      return;
    }
    isSearching.value = true;
    try {
      const results = await client.searchDmMessages(peerJid, trimmed);
      if (requestId === searchRequestId && xmppClient.value === client && activePeerJid.value === peerJid) {
        searchResults.value = results;
      }
    } catch {
      if (requestId === searchRequestId) searchResults.value = [];
    } finally {
      if (requestId === searchRequestId) isSearching.value = false;
    }
  }

  function clearSearch() {
    searchRequestId++;
    searchResults.value = [];
    isSearching.value = false;
  }

  function clearMessages() {
    messageRequestId++;
    messages.value = [];
    isLoadingMessages.value = false;
    clearTypingState();
  }

  function disconnect() {
    messageRequestId++;
    searchRequestId++;
    isLoadingMessages.value = false;
    isSearching.value = false;
    clearTypingState();
  }

  function onIncomingMessage(msg: LiveDmMessage) {
    if (!session.value || !activePeerJid.value || msg.peerJid !== activePeerJid.value) return;
    removeTypingUser(msg.nick);
    if (msg.retractsId) {
      applyRetraction(msg.retractsId);
      return;
    }
    if (msg.replacesId) {
      applyCorrection(msg.replacesId, msg.body, msg.markup);
      return;
    }
    mergeLiveMessage(fromLiveDmMessage(session.value, msg));
  }

  function onChatState(event: DmChatStateEvent) {
    if (!activePeerJid.value || event.peerJid !== activePeerJid.value) return;
    const peerName = peerNameFromJid(event.peerJid);
    if (event.state === "composing") addTypingUser(peerName);
    else removeTypingUser(peerName);
  }

  function onDisplayed(event: DmDisplayedEvent) {
    if (!activePeerJid.value || event.peerJid !== activePeerJid.value) return;
    applyDisplayed(event.messageId, peerNameFromJid(event.peerJid));
  }

  function onReaction(event: DmReactionEvent) {
    if (!activePeerJid.value || event.peerJid !== activePeerJid.value) return;
    applyReaction(event.messageId, peerNameFromJid(event.peerJid), event.emojis);
  }

  return {
    messages,
    draft,
    isLoadingMessages,
    isSending,
    typingUsers,
    timelineEl,
    searchResults,
    isSearching,
    loadMessages,
    sendMessage,
    uploadProgress,
    editMessage,
    retractMessage,
    toggleReaction,
    markDisplayed,
    notifyComposing,
    searchMessages,
    clearSearch,
    clearMessages,
    disconnect,
    onIncomingMessage,
    onChatState,
    onDisplayed,
    onReaction,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
  };
}
