import { computed, nextTick, ref, watch, type Ref } from "vue";
import {
  inferredFileDisposition,
  type DeliveryStatus,
  type ExtensionAnnotationAction,
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
  MessageSearchResult,
  SessionLifecycleEvent,
} from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { MAX_FILE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";
import type { OutboundFileAttachment } from "@/lib/xmpp";
import {
  findMessageById,
  matchMessageId,
  mergeMessageIds,
} from "@/lib/message-ids";
import { findMessageElementById } from "@/lib/message-targeting";
import { isTopPinnedScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { createPinnedEdgeScroller } from "@/lib/pinned-edge-scroll";
import { dmKey, setLastSeen } from "@/lib/last-seen-store";
import {
  listQueuedDmMessages,
} from "@/lib/outbound-queue-store";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import {
  buildDmTimelineFromMamResults,
  fromLiveDmMessage,
  isSameDmCorrectionSender,
  queuedDmMessageToTimeline,
} from "@/dms/message-timeline-state";

export function useDirectMessages(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activePeerJid: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const { mode: scrollDirection } = useScrollDirectionPreference();
  const peerNameFromJid = (jid: string) => barePeerJid(jid).split("@")[0] ?? "unknown";
  const dmLoadErrorMessage = (peerJid: string, opts: { queuedOnly?: boolean } = {}) => {
    const username = peerNameFromJid(peerJid).trim();
    const target = username ? `@${username}` : "this chat";
    return opts.queuedOnly
      ? `Could not load ${target} history. Showing queued messages only. Check the connection and try again.`
      : `Could not load ${target}. Check the connection and try again.`;
  };
  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const isLoadingMessages = ref(false);
  const isLoadingOlderMessages = ref(false);
  const hasOlderMessages = ref(true);
  const isSending = ref(false);
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const timelineEdgeScroller: Ref<((mode: ScrollDirectionMode) => boolean | Promise<boolean>) | null> = ref(null);
  const pinnedEdgeScroller = createPinnedEdgeScroller({
    element: timelineEl,
    mode: scrollDirection,
    virtualScroll: timelineEdgeScroller,
  });
  const typingUsers = ref<string[]>([]);
  const searchResults = ref<MessageSearchResult[]>([]);
  const isSearching = ref(false);
  const uploadProgress = ref({ uploading: false, progress: 0, filename: "" });
  const firstUnseenId = ref<string | null>(null);
  const loadErrorPeerJid = ref<string | null>(null);
  const loadErrorMessage = ref("");
  const latestRemoteMessageId = computed<string | null>(() => {
    for (let i = messages.value.length - 1; i >= 0; i--) {
      const m = messages.value[i];
      if (!m || m.isSelf || m.isRetracted) continue;
      return m.id;
    }
    return null;
  });

  let messageRequestId = 0;
  let oldestArchiveId: string | null = null;
  let initialLatestPagePinned = false;
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
    await pinnedEdgeScroller.scrollToPinnedEdge();
  }

  async function scrollToPinnedEdgeAndPin() {
    return pinnedEdgeScroller.scrollToPinnedEdge({ settle: true });
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
    correctionFromJid: string,
    markup?: LiveDmMessage["markup"],
    references?: LiveDmMessage["references"],
    extensionAnnotations?: LiveDmMessage["extensionAnnotations"],
  ) {
    messages.value = messages.value.map((m) => {
      if (!matchMessageId(m, replacesId) || !isSameDmCorrectionSender(m, correctionFromJid)) return m;
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
      if (extensionAnnotations && extensionAnnotations.length > 0) {
        updated.extensionAnnotations = extensionAnnotations;
      } else {
        delete updated.extensionAnnotations;
      }
      return updated;
    });
  }

  function buildTimelineFromMamResults(
    mamResults: LiveDmMessage[],
    existing: TimelineMessage[] = [],
  ): TimelineMessage[] {
    if (!session.value) return existing;
    return buildDmTimelineFromMamResults({
      session: session.value,
      mamResults,
      existing,
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
    void scrollToPinnedEdgeAndPin();
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

  /** XEP-0198: the XMPP client gave up on the stanza — surface as failed so the
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

  async function loadMessages(peerJid: string, unreadAtLoad = 0) {
    if (!session.value) return;
    const requestId = ++messageRequestId;
    initialLatestPagePinned = false;
    isLoadingMessages.value = true;
    isLoadingOlderMessages.value = false;
    hasOlderMessages.value = true;
    searchRequestId++;
    searchResults.value = [];
    isSearching.value = false;
    oldestArchiveId = null;
    pinnedEdgeScroller.disconnect();
    firstUnseenId.value = null;
    clearActionError();
    loadErrorPeerJid.value = null;
    loadErrorMessage.value = "";
    pendingEchoClientIds.clear();
    messages.value = appendQueuedMessages([], peerJid);
    try {
      const page = xmppClient.value && "queryPersonalMamPage" in xmppClient.value
        ? await xmppClient.value.queryPersonalMamPage(peerJid, 100, { type: "latest" })
        : null;
      const mamResults = page
        ? page.messages
        : xmppClient.value
          ? await xmppClient.value.queryPersonalMam(peerJid, 100)
          : [];
      if (requestId !== messageRequestId || activePeerJid.value !== peerJid) return;
      loadErrorPeerJid.value = null;
      loadErrorMessage.value = "";
      oldestArchiveId = page?.firstArchiveId ?? mamResults[0]?.id ?? null;
      hasOlderMessages.value = page ? !page.complete && !!page.firstArchiveId : mamResults.length >= 100;
      const timeline = buildTimelineFromMamResults(mamResults);
      const timelineWithQueue = appendQueuedMessages(timeline, peerJid);
      messages.value = timelineWithQueue;
      if (requestId === messageRequestId) isLoadingMessages.value = false;

      const key = dmKey(barePeerJid(peerJid));
      const feedTimeline = timelineWithQueue.filter(isFeedVisible);
      firstUnseenId.value = unreadAtLoad > 0 && feedTimeline.length >= unreadAtLoad
        ? feedTimeline[feedTimeline.length - unreadAtLoad]?.id ?? null
        : null;
      const pinned = await scrollToPinnedEdgeAndPin();
      if (!pinned || requestId !== messageRequestId || activePeerJid.value !== peerJid) return;
      initialLatestPagePinned = true;
      const newest = [...timelineWithQueue].reverse().find(isFeedVisible);
      if (newest) setLastSeen(key, newest.id);
    } catch {
      if (requestId === messageRequestId) {
        console.warn("Could not load DM conversation");
        const queuedOnly = appendQueuedMessages([], peerJid);
        messages.value = queuedOnly;
        loadErrorPeerJid.value = peerJid;
        loadErrorMessage.value = dmLoadErrorMessage(peerJid, { queuedOnly: queuedOnly.length > 0 });
        actionError.value = loadErrorMessage.value;
        isLoadingMessages.value = false;
      }
    }
  }

  async function loadOlderMessages() {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    const before = oldestArchiveId;
    if (!client || !peerJid || !before || !initialLatestPagePinned || !hasOlderMessages.value || isLoadingOlderMessages.value) {
      return;
    }
    if (!("queryPersonalMamPage" in client)) return;
    const requestId = messageRequestId;
    const isCurrentRequest = () =>
      requestId === messageRequestId &&
      xmppClient.value === client &&
      activePeerJid.value === peerJid;
    const el = timelineEl.value;
    const previousHeight = el?.scrollHeight ?? 0;
    const previousTop = el?.scrollTop ?? 0;
    isLoadingOlderMessages.value = true;
    try {
      const page = await client.queryPersonalMamPage(peerJid, 100, { type: "before", before });
      if (!isCurrentRequest()) return;
      oldestArchiveId = page.firstArchiveId ?? oldestArchiveId;
      hasOlderMessages.value = !page.complete && !!page.firstArchiveId && page.firstArchiveId !== before;
      const withoutQueued = messages.value.filter((m) => !(m.isSelf && m.deliveryStatus === "queued"));
      messages.value = appendQueuedMessages(buildTimelineFromMamResults(page.messages, withoutQueued), peerJid);
      await nextTick();
      if (el && !isTopPinnedScrollDirection(scrollDirection.value)) {
        el.scrollTop = previousTop + (el.scrollHeight - previousHeight);
      }
    } catch {
      if (isCurrentRequest()) {
        console.warn("Could not load older DM messages");
        actionError.value = dmLoadErrorMessage(peerJid);
      }
    } finally {
      if (isCurrentRequest()) isLoadingOlderMessages.value = false;
    }
  }

  async function ensureMessageLoaded(messageId: string): Promise<boolean> {
    if (findMessageById(messages.value, messageId)) return true;
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid || !("queryPersonalMamPage" in client)) return false;

    let before = oldestArchiveId;
    while (before && hasOlderMessages.value && !findMessageById(messages.value, messageId)) {
      const requestId = messageRequestId;
      const previousBefore = before;
      const page = await client.queryPersonalMamPage(peerJid, 100, { type: "before", before });
      if (
        requestId !== messageRequestId ||
        xmppClient.value !== client ||
        activePeerJid.value !== peerJid
      ) {
        return false;
      }
      const nextBefore = page.firstArchiveId ?? previousBefore;
      oldestArchiveId = nextBefore;
      hasOlderMessages.value = !page.complete && !!page.firstArchiveId && page.firstArchiveId !== previousBefore;
      const withoutQueued = messages.value.filter((m) => !(m.isSelf && m.deliveryStatus === "queued"));
      messages.value = appendQueuedMessages(buildTimelineFromMamResults(page.messages, withoutQueued), peerJid);
      if (findMessageById(messages.value, messageId)) return true;
      if (!page.firstArchiveId || page.firstArchiveId === previousBefore || page.complete) break;
      before = nextBefore;
    }
    return !!findMessageById(messages.value, messageId);
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
    const targetId = msg?.replyableId ?? msg?.id ?? messageId;
    const myNick = session.value.username;
    const currentReactions = msg?.reactions ?? {};
    const previousEmojis: string[] = [];
    for (const [e, nicks] of Object.entries(currentReactions)) {
      if (nicks.includes(myNick)) previousEmojis.push(e);
    }
    const myEmojis = new Set(previousEmojis);
    if (myEmojis.has(emoji)) myEmojis.delete(emoji);
    else myEmojis.add(emoji);

    const nextEmojis = [...myEmojis];
    // Optimistic local update: the sender device never receives an XEP-0280
    // carbon of its own send, so without this the reaction stays invisible
    // to its author until the next MAM reload.
    applyReaction(targetId, myNick, nextEmojis);

    try {
      await xmppClient.value.sendDmReaction(activePeerJid.value, targetId, nextEmojis);
    } catch (e) {
      // Roll back the optimistic update — no echo or carbon will arrive
      // to reconcile a failed send.
      applyReaction(targetId, myNick, previousEmojis);
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

  async function invokeExtensionAction(action: ExtensionAnnotationAction) {
    const client = xmppClient.value;
    if (!client) {
      const error = new Error("XMPP session is not ready.");
      actionError.value = normalizeError(error);
      throw error;
    }
    if (!action.launch) {
      const error = new Error("This extension action is missing launch metadata.");
      actionError.value = normalizeError(error);
      throw error;
    }
    clearActionError();
    try {
      return await client.invokeExtensionLaunch(action.launch);
    } catch (e) {
      actionError.value = normalizeError(e);
      throw e;
    }
  }

  async function editMessage(messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]) {
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
    clearActionError();
    try {
      const results = await client.searchDmMessages(peerJid, trimmed);
      if (requestId === searchRequestId && xmppClient.value === client && activePeerJid.value === peerJid) {
        searchResults.value = results;
      }
    } catch (e) {
      if (requestId === searchRequestId) {
        searchResults.value = [];
        actionError.value = normalizeError(e);
      }
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
    searchRequestId++;
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    initialLatestPagePinned = false;
    oldestArchiveId = null;
    hasOlderMessages.value = true;
    isLoadingOlderMessages.value = false;
    messages.value = [];
    isLoadingMessages.value = false;
    searchResults.value = [];
    isSearching.value = false;
    firstUnseenId.value = null;
    clearTypingState();
  }

  function disconnect() {
    messageRequestId++;
    searchRequestId++;
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    initialLatestPagePinned = false;
    oldestArchiveId = null;
    hasOlderMessages.value = true;
    isLoadingOlderMessages.value = false;
    isLoadingMessages.value = false;
    isSearching.value = false;
    searchResults.value = [];
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
      applyCorrection(
        msg.replacesId,
        msg.body,
        msg.fromJid,
        msg.markup,
        msg.references,
        msg.extensionAnnotations,
      );
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
    // Attribute the reaction to whoever sent the stanza, not the
    // conversation key. For self-sent carbon-forwarded reactions
    // `peerJid` is normalized to the recipient, so deriving the nick
    // from `peerJid` would wrongly credit the partner.
    applyReaction(event.messageId, peerNameFromJid(event.reactorJid), event.emojis);
  }

  watch(scrollDirection, () => {
    void alignTimelineToPreference();
  });

  watch(
    [timelineEl, timelineEdgeScroller],
    async ([el, edgeScroller]) => {
      if (!el || !edgeScroller || isLoadingMessages.value) return;
      if (!messages.value.some(isFeedVisible)) return;
      const requestId = messageRequestId;
      const peerJid = activePeerJid.value;
      const pinned = await scrollToPinnedEdgeAndPin();
      if (
        pinned &&
        requestId === messageRequestId &&
        activePeerJid.value === peerJid &&
        messages.value.some(isFeedVisible)
      ) {
        initialLatestPagePinned = true;
        const newest = [...messages.value].reverse().find(isFeedVisible);
        if (peerJid && newest) setLastSeen(dmKey(barePeerJid(peerJid)), newest.id);
      }
    },
    { flush: "post" },
  );

  return {
    messages,
    firstUnseenId,
    draft,
    isLoadingMessages,
    isLoadingOlderMessages,
    hasOlderMessages,
    isSending,
    typingUsers,
    timelineEl,
    timelineEdgeScroller,
    searchResults,
    isSearching,
    loadErrorPeerJid,
    loadErrorMessage,
    loadMessages,
    loadOlderMessages,
    ensureMessageLoaded,
    sendMessage,
    uploadProgress,
    editMessage,
    retractMessage,
    invokeExtensionAction,
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
    isPinnedAtEdge: pinnedEdgeScroller.isPinnedAtEdge,
    latestRemoteMessageId,
  };
}
