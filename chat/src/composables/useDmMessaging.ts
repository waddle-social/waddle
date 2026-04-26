import { nextTick, ref, watch, type Ref } from "vue";
import {
  inferredFileDisposition,
  type DeliveryStatus,
  type MarkupSpan,
  type MessageReference,
  type TimelineMessage,
} from "@/lib/chat-ui";
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
import { MAX_FILE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";
import type { OutboundFileAttachment } from "@/lib/xmpp";
import {
  findMessageById,
  indexMessageByIds,
  matchMessageId,
  mergeMessageIds,
} from "@/lib/message-ids";
import { findMessageElementById } from "@/lib/message-targeting";
import { getPinnedScrollTop, isTopPinnedScrollDirection } from "@/lib/scroll-direction";
import { dmKey, getLastSeen, setLastSeen } from "@/lib/last-seen-store";
import {
  listQueuedDmMessages,
  type PersistedQueuedDmMessage,
} from "@/lib/outbound-queue-store";
import { useScrollDirection } from "@/composables/useScrollDirection";

function fromLiveDmMessage(
  session: WaddleSession,
  msg: LiveDmMessage,
  parentLookup?: (id: string) => { body?: string } | undefined,
): TimelineMessage {
  const tm: TimelineMessage = {
    id: msg.id,
    author: msg.nick,
    authorJid: msg.fromJid,
    body: msg.body,
    createdAt: msg.createdAt,
    isSelf: barePeerJid(msg.fromJid) === barePeerJid(session.jid),
  };
  if (msg.wireIds?.length) tm.wireIds = msg.wireIds;
  if (msg.mentions?.length) tm.mentions = msg.mentions;
  if (msg.markup?.length) tm.markup = msg.markup;
  if (msg.references?.length) tm.references = msg.references;
  if (msg.sharedFiles && msg.sharedFiles.length > 0) tm.sharedFiles = msg.sharedFiles;
  if (msg.githubEmbeds && msg.githubEmbeds.length > 0) tm.githubEmbeds = msg.githubEmbeds;
  if (msg.isSticker) tm.isSticker = true;
  if (msg.replyTo) {
    const parent = parentLookup?.(msg.replyTo.id);
    tm.replyTo = {
      id: msg.replyTo.id,
      ...(msg.replyTo.author ? { author: msg.replyTo.author } : {}),
      ...(parent?.body ? { preview: parent.body } : {}),
    };
  }
  if (msg.threadId) tm.threadId = msg.threadId;
  if (msg.parentThreadId) tm.parentThreadId = msg.parentThreadId;
  return tm;
}

function queuedDmMessageToTimeline(
  session: WaddleSession,
  queued: PersistedQueuedDmMessage,
): TimelineMessage {
  const message: TimelineMessage = {
    id: queued.id,
    author: session.username,
    authorJid: session.jid,
    body: queued.body || (queued.files?.[0]?.url ?? ""),
    createdAt: queued.createdAt,
    isSelf: true,
    deliveryStatus: "queued",
  };
  if (queued.markup?.length) message.markup = queued.markup;
  if (queued.references?.length) message.references = queued.references;
  if (queued.replyTo) {
    message.replyTo = {
      id: queued.replyTo.id,
      ...(queued.replyTo.author ? { author: queued.replyTo.author } : {}),
      ...(queued.replyTo.body ? { preview: queued.replyTo.body } : {}),
    };
  }
  if (queued.threadId) message.threadId = queued.threadId;
  if (queued.parentThreadId) message.parentThreadId = queued.parentThreadId;
  if (queued.files && queued.files.length > 0) {
    message.sharedFiles = queued.files.map((file) => ({
      url: file.url,
      name: file.name,
      mediaType: file.mediaType,
      size: file.size,
      ...(file.width ? { width: file.width } : {}),
      ...(file.height ? { height: file.height } : {}),
      disposition: inferredFileDisposition(file.mediaType, file.name ?? file.url),
      ...(file.encrypted ? { encrypted: file.encrypted } : {}),
    }));
  }
  return message;
}

export function useDmMessaging(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activePeerJid: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const { mode: scrollDirection } = useScrollDirection();
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
  const firstUnseenId = ref<string | null>(null);

  let messageRequestId = 0;
  let searchRequestId = 0;
  let lastChatState: ChatStateType = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  // Client-assigned stanza ids still awaiting server-echo reconciliation.
  // Only these participate in the body-based fallback match so repeated
  // identical text doesn't mis-target already-reconciled messages.
  const pendingEchoClientIds = new Set<string>();

  function queuedMessagesForPeer(peerJid: string): TimelineMessage[] {
    const currentSession = session.value;
    if (!currentSession) return [];
    const queued = listQueuedDmMessages(barePeerJid(currentSession.jid), barePeerJid(peerJid));
    for (const message of queued) pendingEchoClientIds.add(message.id);
    return queued.map((message) => queuedDmMessageToTimeline(currentSession, message));
  }

  function appendQueuedMessages(timeline: TimelineMessage[], peerJid: string): TimelineMessage[] {
    const queued = queuedMessagesForPeer(peerJid).filter((message) => !findMessageById(timeline, message.id));
    return queued.length > 0 ? [...timeline, ...queued] : timeline;
  }

  async function scrollToPinnedEdge() {
    await nextTick();
    await nextTick();
    const el = timelineEl.value;
    if (!el) return;
    el.scrollTop = getPinnedScrollTop(el, scrollDirection.value);
  }

  // Initial-load variant: re-pin for ~500ms so late layout (images, avatars,
  // markup reflow) doesn't strand the user above the newest message. Not
  // used per-message — ResizeObserver allocation per live message would be
  // O(n) overhead. Same pattern as useMessaging.ts.
  async function scrollToPinnedEdgeAndPin() {
    await scrollToPinnedEdge();
    const el = timelineEl.value;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      el.scrollTop = getPinnedScrollTop(el, scrollDirection.value);
    });
    observer.observe(el);
    for (const child of Array.from(el.children)) {
      observer.observe(child);
    }
    setTimeout(() => observer.disconnect(), 500);
  }

  async function scrollFirstUnseenIntoView(messageId: string) {
    if (isTopPinnedScrollDirection(scrollDirection.value)) {
      await scrollToPinnedEdgeAndPin();
      return;
    }
    await nextTick();
    await nextTick();
    const el = timelineEl.value;
    if (!el) return;
    // Prefer the divider so it stays on-screen; message-level fallback only
    // if the divider hasn't rendered yet.
    const divider = el.querySelector("[data-new-messages-divider]");
    if (divider && typeof (divider as HTMLElement).scrollIntoView === "function") {
      (divider as HTMLElement).scrollIntoView({ block: "start" });
      return;
    }
    const target = findMessageElementById(el, messageId);
    if (target && typeof (target as HTMLElement).scrollIntoView === "function") {
      (target as HTMLElement).scrollIntoView({ block: "start" });
    }
  }

  async function alignTimelineToPreference() {
    if (firstUnseenId.value) {
      await scrollFirstUnseenIntoView(firstUnseenId.value);
      return;
    }
    await scrollToPinnedEdgeAndPin();
  }

  function isFeedVisible(m: TimelineMessage): boolean {
    return !m.threadId || m.id === m.threadId;
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
      if (!matchMessageId(m, messageId)) return m;
      const existing = m.readBy ? [...m.readBy] : [];
      if (!existing.includes(nick)) existing.push(nick);
      return { ...m, readBy: existing };
    });
  }

  function applyReaction(messageId: string, nick: string, emojis: string[]) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (!matchMessageId(m, messageId)) return m;
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
    messages.value = messages.value.map((m) => (matchMessageId(m, retractsId) ? { ...m, body: "", isRetracted: true } : m));
  }

  function applyCorrection(
    replacesId: string,
    newBody: string,
    markup?: LiveDmMessage["markup"],
    references?: LiveDmMessage["references"],
    githubEmbeds?: LiveDmMessage["githubEmbeds"],
  ) {
    messages.value = messages.value.map((m) => {
      if (!matchMessageId(m, replacesId)) return m;
      const updated: TimelineMessage = { ...m, body: newBody.trim(), isEdited: true };
      if (markup && markup.length > 0) {
        updated.markup = markup;
      } else {
        delete updated.markup;
      }
      if (references && references.length > 0) {
        updated.references = references;
      } else {
        delete updated.references;
      }
      if (githubEmbeds && githubEmbeds.length > 0) {
        updated.githubEmbeds = githubEmbeds;
      } else if (m.githubEmbeds?.length) {
        const newBodyText = newBody.trim();
        const surviving = m.githubEmbeds.filter((e) => newBodyText.includes(e.url));
        if (surviving.length > 0) {
          updated.githubEmbeds = surviving;
        } else {
          delete updated.githubEmbeds;
        }
      } else {
        delete updated.githubEmbeds;
      }
      return updated;
    });
  }

  function mergeLiveMessage(msg: TimelineMessage) {
    // Self-echo reconciliation: match by id first; otherwise body-match only
    // against messages still awaiting reconciliation so duplicates don't
    // retarget already-reconciled entries. Echo = authoritative → delivered.
    const existingById = [msg.id, ...(msg.wireIds ?? [])]
      .map((id) => findMessageById(messages.value, id))
      .find((message): message is TimelineMessage => !!message);
    const pendingSelfEcho = messages.value.find(
      (m) => pendingEchoClientIds.has(m.id) && m.isSelf && msg.isSelf && m.body === msg.body,
    );
    const preservedSelfEcho = [...messages.value].reverse().find(
      (m) =>
        m.isSelf
        && msg.isSelf
        && m.body === msg.body
        && !!m.deliveryStatus
        && m.deliveryStatus !== "delivered",
    );
    const existing = existingById ?? pendingSelfEcho ?? preservedSelfEcho;
    if (existing) {
      const wasPending = existing.id !== msg.id;
      messages.value = messages.value.map((m) => {
        if (m.id !== existing.id) return m;
        const updated: TimelineMessage = {
          ...m,
          ...msg,
          ...mergeMessageIds(m, msg.id, msg.wireIds),
        };
        if (m.isSelf && msg.isSelf) {
          updated.deliveryStatus = "delivered" as DeliveryStatus;
        }
        return updated;
      });
      if (wasPending) pendingEchoClientIds.delete(existing.id);
      return;
    }
    const peerJid = activePeerJid.value;
    messages.value = [...messages.value, msg];
    // Always snaps to the active edge, so last-seen advances in lockstep.
    void scrollToPinnedEdge();
    if (peerJid && isFeedVisible(msg)) {
      setLastSeen(dmKey(barePeerJid(peerJid)), msg.id);
    }
  }

  /** XEP-0198: SM ack promotes the matching self-sent message to delivered. */
  function onMessageAck(messageId: string) {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus !== "delivered"
        ? { ...m, deliveryStatus: "delivered" as DeliveryStatus }
        : m,
    );
  }

  function onMessageQueueStatus(messageId: string, status: "queued" | "sending") {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus !== "delivered"
        ? { ...m, deliveryStatus: status as DeliveryStatus }
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
        m.isSelf && (
          m.deliveryStatus === "queued"
          || m.deliveryStatus === "sending"
          || m.deliveryStatus === "failed"
        ),
    );
    void (async () => {
      await loadMessages(peerJid);
      if (preserved.length === 0 || activePeerJid.value !== peerJid) return;
      const toAppend = preserved.filter((m) => !findMessageById(messages.value, m.id));
      if (toAppend.length > 0) messages.value = [...messages.value, ...toAppend];
    })();
  }

  async function loadMessages(peerJid: string) {
    if (!session.value) return;
    const requestId = ++messageRequestId;
    isLoadingMessages.value = true;
    firstUnseenId.value = null;
    clearActionError();
    pendingEchoClientIds.clear();
    messages.value = appendQueuedMessages([], peerJid);
    try {
      const mamResults = xmppClient.value ? await xmppClient.value.queryPersonalMam(peerJid, 100) : [];
      if (requestId !== messageRequestId || activePeerJid.value !== peerJid) return;
      const regular: LiveDmMessage[] = [];
      const reactionUpdates: { targetId: string; nick: string; emojis: string[] }[] = [];
      const retractionUpdates: string[] = [];
      const correctionUpdates: {
        targetId: string;
        body: string;
        markup?: LiveDmMessage["markup"];
        references?: LiveDmMessage["references"];
        githubEmbeds?: LiveDmMessage["githubEmbeds"];
      }[] = [];
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
            references: msg.references,
            githubEmbeds: msg.githubEmbeds,
          });
        } else if (msg.body || (msg.sharedFiles && msg.sharedFiles.length > 0) || msg.isSticker || (msg.githubEmbeds && msg.githubEmbeds.length > 0)) {
          regular.push(msg);
        }
      }
      const byId = new Map<string, TimelineMessage>();
      const timeline = regular.map((m) => {
        const tm = fromLiveDmMessage(session.value!, m, (id) => byId.get(id));
        indexMessageByIds(byId, tm);
        return tm;
      });
      for (const update of correctionUpdates) {
        const target = findMessageById(timeline, update.targetId);
        if (!target) continue;
        target.body = update.body;
        target.isEdited = true;
        if (update.markup && update.markup.length > 0) {
          target.markup = update.markup;
        } else {
          delete target.markup;
        }
        if (update.references && update.references.length > 0) {
          target.references = update.references;
        } else {
          delete target.references;
        }
        if (update.githubEmbeds && update.githubEmbeds.length > 0) {
          target.githubEmbeds = update.githubEmbeds;
        } else if (target.githubEmbeds?.length) {
          const surviving = target.githubEmbeds.filter((e) => update.body.includes(e.url));
          if (surviving.length > 0) {
            target.githubEmbeds = surviving;
          } else {
            delete target.githubEmbeds;
          }
        } else {
          delete target.githubEmbeds;
        }
      }
      for (const retractsId of retractionUpdates) {
        const target = findMessageById(timeline, retractsId);
        if (!target) continue;
        target.body = "";
        target.isRetracted = true;
      }
      for (const update of reactionUpdates) {
        const target = findMessageById(timeline, update.targetId);
        if (!target) continue;
        const reactions: Record<string, string[]> = target.reactions ? { ...target.reactions } : {};
        for (const emoji of update.emojis) {
          if (!reactions[emoji]) reactions[emoji] = [];
          if (!reactions[emoji].includes(update.nick)) reactions[emoji].push(update.nick);
        }
        target.reactions = reactions;
      }
      const timelineWithQueue = appendQueuedMessages(timeline, peerJid);
      messages.value = timelineWithQueue;
      if (requestId === messageRequestId) isLoadingMessages.value = false;

      // See useMessaging.loadMessages — same last-seen anchor semantics,
      // keyed by peer bare JID. Anchor is computed against feed-visible
      // messages so it always matches something ContentArea renders.
      const key = dmKey(barePeerJid(peerJid));
      const lastSeenId = getLastSeen(key);
      const lastSeenIdx = lastSeenId
        ? timelineWithQueue.findIndex((m) => matchMessageId(m, lastSeenId))
        : -1;
      const firstUnseen =
        lastSeenIdx !== -1 && lastSeenIdx < timelineWithQueue.length - 1
          ? timelineWithQueue.slice(lastSeenIdx + 1).find(isFeedVisible)
          : undefined;
      if (firstUnseen) {
        firstUnseenId.value = firstUnseen.id;
        await scrollFirstUnseenIntoView(firstUnseen.id);
      } else {
        firstUnseenId.value = null;
        await scrollToPinnedEdgeAndPin();
        const newest = [...timelineWithQueue].reverse().find(isFeedVisible);
        if (newest) setLastSeen(key, newest.id);
      }
    } catch (e) {
      if (requestId === messageRequestId) {
        const queuedOnly = appendQueuedMessages([], peerJid);
        messages.value = queuedOnly;
        actionError.value = queuedOnly.length > 0 ? "" : normalizeError(e);
        isLoadingMessages.value = false;
      }
    }
  }

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
          void scrollToPinnedEdge();
        }
        if (fromComposer) draft.value = "";
        if (composingTimeout) {
          clearTimeout(composingTimeout);
          composingTimeout = null;
        }
        lastChatState = "active";
      }
      if (result?.state === "sending") {
        void client.sendDmChatState(peerJid, "active").catch(() => undefined);
      }
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSending.value = false;
      uploadProgress.value = { uploading: false, progress: 0, filename: "" };
    }
  }

  async function toggleReaction(messageId: string, emoji: string) {
    if (!xmppClient.value || !activePeerJid.value || !session.value) return;
    const msg = findMessageById(messages.value, messageId);
    const targetId = msg?.id ?? messageId;
    const myNick = session.value.username;
    const currentReactions = msg?.reactions ?? {};
    const myEmojis = new Set<string>();
    for (const [e, nicks] of Object.entries(currentReactions)) {
      if (nicks.includes(myNick)) myEmojis.add(e);
    }
    if (myEmojis.has(emoji)) myEmojis.delete(emoji);
    else myEmojis.add(emoji);
    try {
      await xmppClient.value.sendDmReaction(activePeerJid.value, targetId, [...myEmojis]);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activePeerJid.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    clearActionError();
    try {
      await xmppClient.value.sendDmRetraction(activePeerJid.value, targetId);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function editMessage(messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]) {
    if (!xmppClient.value || !activePeerJid.value || !newBody.trim()) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    clearActionError();
    try {
      await xmppClient.value.sendDmCorrection(activePeerJid.value, newBody, targetId, markup, references);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activePeerJid.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    void xmppClient.value.sendDmDisplayed(activePeerJid.value, targetId).catch(() => undefined);
  }

  function notifyComposing() {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid) return;
    if (lastChatState !== "composing") {
      lastChatState = "composing";
      void client.sendDmChatState(peerJid, "composing").catch(() => undefined);
    }
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (xmppClient.value !== client || activePeerJid.value !== peerJid) return;
      lastChatState = "paused";
      void client.sendDmChatState(peerJid, "paused").catch(() => undefined);
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
    pendingEchoClientIds.clear();
    messages.value = [];
    isLoadingMessages.value = false;
    firstUnseenId.value = null;
    clearTypingState();
  }

  function disconnect() {
    messageRequestId++;
    searchRequestId++;
    pendingEchoClientIds.clear();
    isLoadingMessages.value = false;
    isSearching.value = false;
    firstUnseenId.value = null;
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
      applyCorrection(msg.replacesId, msg.body, msg.markup, msg.references, msg.githubEmbeds);
      return;
    }
    mergeLiveMessage(
      fromLiveDmMessage(session.value, msg, (id) => findMessageById(messages.value, id)),
    );
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

  watch(scrollDirection, () => {
    void alignTimelineToPreference();
  });

  return {
    messages,
    firstUnseenId,
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
    onMessageQueueStatus,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
    scrollToPinnedEdge,
  };
}
