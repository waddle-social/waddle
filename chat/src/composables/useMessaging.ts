import { ref, computed, nextTick, watch, type Ref } from "vue";
import type { WaddleApi } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  roomBareJidFor,
  type LiveRoomMessage,
  type RoomActivityEvent,
  type SessionLifecycleEvent,
  type XmppStatusSnapshot,
  type ChatStateType,
  type RoomHats,
  type RoomPresence,
} from "@/lib/xmpp-client";
import type { DeliveryStatus, MarkupSpan, TimelineMessage } from "@/lib/chat-ui";
import { MAX_IMAGE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";
import type { OutboundFileAttachment } from "@/lib/xmpp";

export function fromLiveMessage(
  session: WaddleSession,
  msg: LiveRoomMessage,
  parentLookup?: (id: string) => { body?: string } | undefined,
): TimelineMessage {
  const tm: TimelineMessage = {
    id: msg.id,
    author: msg.nick,
    authorJid: `${msg.roomJid}/${msg.nick}`,
    body: msg.body,
    createdAt: msg.createdAt,
    isSelf: msg.nick === session.username,
  };
  if (msg.mentions && msg.mentions.length > 0) {
    tm.mentions = msg.mentions;
  }
  if (msg.sharedFiles && msg.sharedFiles.length > 0) {
    tm.sharedFiles = msg.sharedFiles;
  }
  if (msg.isSticker) {
    tm.isSticker = true;
  }
  if (msg.broadcastMention) {
    tm.broadcastMention = msg.broadcastMention;
  }
  if (msg.markup && msg.markup.length > 0) {
    tm.markup = msg.markup;
  }
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

export function formatStamp(value: string) {
  return new Intl.DateTimeFormat("en", {
    hour: "2-digit",
    minute: "2-digit",
    month: "short",
    day: "numeric",
  }).format(new Date(value));
}

export function useMessaging(
  session: Ref<WaddleSession | null>,
  _api: Ref<WaddleApi | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activeWaddleId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const xmppStatus = ref<XmppStatusSnapshot>({
    state: "offline",
    detail: "Live room offline",
  });

  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const isLoadingMessages = ref(false);
  const isSending = ref(false);
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const typingUsers = ref<string[]>([]);
  const roomHats = ref<RoomHats>({});
  const roomPresence = ref<RoomPresence>({});
  const roomLastSeen = ref<Record<string, number>>({});
  const slowModeCooldown = ref(0);
  const uploadProgress = ref({ uploading: false, progress: 0, filename: "" });
  const activeChannels = ref<Set<string>>(new Set());
  const lastMentionActivity = ref<RoomActivityEvent | null>(null);
  const roomAvatarHashes = ref<Record<string, string>>({});
  const searchQuery = ref("");
  const searchResults = ref<{ id: string; nick: string; body: string; createdAt: string }[]>([]);
  const isSearching = ref(false);
  let slowModeTimer: ReturnType<typeof setInterval> | null = null;

  let messageRequestId = 0;
  let searchRequestId = 0;
  let lastChatState: ChatStateType = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  // Client-assigned stanza ids still awaiting MUC-echo reconciliation. A self
  // echo with a server-rewritten id falls back to body matching, but only
  // against messages in this set — otherwise sending the same text twice
  // would cause the second echo to rewrite the first (already reconciled)
  // message's id.
  const pendingEchoClientIds = new Set<string>();

  const currentRoomJid = computed(() => {
    if (!session.value || !activeWaddleId.value || !activeChannelId.value) return null;
    return roomBareJidFor(session.value, activeWaddleId.value, activeChannelId.value);
  });

  watch(xmppClient, (client) => {
    if (client) {
      client.setMessageHandler((msg) => {
        if (
          !currentRoomJid.value ||
          msg.roomJid !== currentRoomJid.value ||
          msg.type !== "message"
        )
          return;
        // When a user sends a real message, clear their typing state
        removeTypingUser(msg.nick);

        // XEP-0424: Handle message retractions
        if (msg.retractsId) {
          applyRetraction(msg.retractsId);
          return;
        }

        // XEP-0308: Handle message corrections
        if (msg.replacesId) {
          applyCorrection(msg.replacesId, msg.body, msg.markup);
          return;
        }

        mergeLiveMessage(
          fromLiveMessage(session.value!, msg, (id) => messages.value.find((m) => m.id === id)),
        );

        // Trigger notification for mentions when tab is unfocused
        const isTabHidden = typeof document !== "undefined"
          && (!document.hasFocus() || document.visibilityState === "hidden");
        if (msg.nick !== session.value?.username && isTabHidden) {
          const isMentioned =
            !!msg.broadcastMention ||
            msg.mentions?.some(
              (m) => m === session.value?.username || m.split("@")[0] === session.value?.username,
            );
          if (isMentioned) {
            const activity: RoomActivityEvent = {
              roomJid: msg.roomJid,
              nick: msg.nick,
              body: msg.body,
            };
            if (msg.mentions) activity.mentions = msg.mentions;
            if (msg.broadcastMention) activity.broadcastMention = msg.broadcastMention;
            lastMentionActivity.value = activity;
          }
        }
      });
      client.setStatusHandler((status) => {
        xmppStatus.value = status;
      });
      client.setChatStateHandler((event) => {
        if (!currentRoomJid.value || event.roomJid !== currentRoomJid.value) return;
        if (event.state === "composing") {
          addTypingUser(event.nick);
        } else {
          removeTypingUser(event.nick);
        }
      });
      client.setReactionHandler((event) => {
        if (!currentRoomJid.value || event.roomJid !== currentRoomJid.value) return;
        applyReaction(event.messageId, event.nick, event.emojis);
      });
      client.setDisplayedHandler((event) => {
        if (!currentRoomJid.value || event.roomJid !== currentRoomJid.value) return;
        applyDisplayed(event.messageId, event.nick);
      });
      client.setHatsHandler((hats) => {
        roomHats.value = hats;
      });
      client.setPresenceHandler((presence) => {
        if (Object.keys(presence).length === 0) {
          roomLastSeen.value = {};
        }
        roomPresence.value = presence;
      });
      client.setLastSeenHandler((nick, timestamp) => {
        roomLastSeen.value = { ...roomLastSeen.value, [nick]: timestamp };
      });
      client.setActivityHandler((event) => {
        activeChannels.value = new Set([...activeChannels.value, event.roomJid]);
        if (event.mentions?.length || event.broadcastMention) {
          lastMentionActivity.value = event;
        }
      });
      // XEP-0486: Track room avatar hashes from presence
      client.setRoomAvatarHandler((roomJid, hash) => {
        roomAvatarHashes.value = { ...roomAvatarHashes.value, [roomJid]: hash };
      });
      client.setSlowModeHandler((seconds) => {
        slowModeCooldown.value = seconds;
        if (slowModeTimer) clearInterval(slowModeTimer);
        slowModeTimer = setInterval(() => {
          slowModeCooldown.value--;
          if (slowModeCooldown.value <= 0) {
            slowModeCooldown.value = 0;
            if (slowModeTimer) {
              clearInterval(slowModeTimer);
              slowModeTimer = null;
            }
          }
        }, 1000);
      });
    } else {
      xmppStatus.value = { state: "offline", detail: "Live room offline" };
      clearTypingState();
    }
  });

  function addTypingUser(nick: string) {
    if (!typingUsers.value.includes(nick)) {
      typingUsers.value = [...typingUsers.value, nick];
    }
    // Auto-expire after 5 seconds (in case we miss a "paused" notification)
    const existing = typingTimers.get(nick);
    if (existing) clearTimeout(existing);
    typingTimers.set(
      nick,
      setTimeout(() => removeTypingUser(nick), 5000),
    );
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

  function notifyComposing() {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!client || !waddleId || !channelId) return;

    if (lastChatState !== "composing") {
      lastChatState = "composing";
      client.sendChatState(waddleId, channelId, "composing");
    }

    // Reset the pause timer: if user stops typing for 3s, send "paused"
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (
        xmppClient.value !== client ||
        activeWaddleId.value !== waddleId ||
        activeChannelId.value !== channelId
      )
        return;
      lastChatState = "paused";
      client.sendChatState(waddleId, channelId, "paused");
    }, 3000);
  }

  async function scrollToBottom() {
    await nextTick();
    // Double nextTick ensures the DOM has fully rendered after reactive updates
    await nextTick();
    if (timelineEl.value) {
      timelineEl.value.scrollTop = timelineEl.value.scrollHeight;
    }
  }

  function applyDisplayed(messageId: string, nick: string) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.id !== messageId) return m;
      const existing = m.readBy ? [...m.readBy] : [];
      if (!existing.includes(nick)) {
        existing.push(nick);
      }
      return { ...m, readBy: existing };
    });
  }

  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;
    xmppClient.value.sendDisplayed(activeWaddleId.value, activeChannelId.value, messageId);
  }

  function applyReaction(messageId: string, nick: string, emojis: string[]) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.id !== messageId) return m;
      const existing: Record<string, string[]> = m.reactions ? { ...m.reactions } : {};
      // Remove this nick from all existing emoji lists
      for (const key of Object.keys(existing)) {
        existing[key] = (existing[key] ?? []).filter((n) => n !== nick);
        if (existing[key].length === 0) delete existing[key];
      }
      // Add the nick to each new emoji
      for (const emoji of emojis) {
        if (!existing[emoji]) existing[emoji] = [];
        existing[emoji].push(nick);
      }
      const updated = { ...m };
      if (Object.keys(existing).length > 0) {
        updated.reactions = existing;
      } else {
        delete updated.reactions;
      }
      return updated;
    });
  }

  function applyRetraction(retractsId: string) {
    messages.value = messages.value.map((m) =>
      m.id === retractsId ? { ...m, body: "", isRetracted: true } : m,
    );
  }

  function applyCorrection(replacesId: string, newBody: string, markup?: MarkupSpan[]) {
    const idx = messages.value.findIndex((m) => m.id === replacesId);
    if (idx === -1) return;
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
    // Check if this is a self-echo reconciling an optimistically-inserted
    // message. Match by ID first; otherwise, for our own sends, fall back
    // to body matching — but only against messages still awaiting echo
    // reconciliation (tracked via pendingEchoClientIds). Without that
    // constraint, sending the same text twice would let the second echo
    // re-target the already-reconciled first message.
    const existing = messages.value.find(
      (m) =>
        m.id === msg.id ||
        (pendingEchoClientIds.has(m.id) && m.isSelf && msg.isSelf && m.body === msg.body),
    );
    if (existing) {
      const wasPending = existing.id !== msg.id;
      if (wasPending || existing.deliveryStatus === "sending") {
        // Reconcile the ID to the server-assigned one and mark delivered.
        // A self-echo is authoritative evidence that the server accepted
        // the stanza, so it supersedes any prior "sending" or "failed"
        // optimistic state.
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

  /**
   * XEP-0198: server acked our outbound stanza. The id here is the
   * client-assigned stanza ID. Promote the matching optimistic entry to
   * "delivered" even if the MUC self-echo hasn't arrived yet (the echo
   * will later reconcile the ID through mergeLiveMessage).
   */
  function onMessageAck(messageId: string) {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus === "sending"
        ? { ...m, deliveryStatus: "delivered" as DeliveryStatus }
        : m,
    );
  }

  /**
   * XEP-0198: stanza.js gave up on the stanza (resume failed or no
   * resumable transport). Mark the message as failed so the UI can
   * surface a retry affordance. Kept in place so the user can see what
   * did not go through.
   */
  function onMessageDeliveryFailure(messageId: string) {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus !== "delivered"
        ? { ...m, deliveryStatus: "failed" as DeliveryStatus }
        : m,
    );
  }

  /**
   * On a fresh XMPP session (resume failed or first connect after a drop),
   * refetch MAM to close any message gap for the current channel. Local
   * optimistic sends (sending/failed) are preserved across the reload so
   * the UI doesn't drop unsent entries the user can still retry.
   * Resumed sessions never call this — the server replays everything.
   */
  function onSessionLifecycle(event: SessionLifecycleEvent) {
    if (event.type !== "fresh") return;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!waddleId || !channelId) return;
    // Only catch up if we had already loaded this channel; otherwise the
    // standard loadMessages call on channel-select handles it.
    if (messages.value.length === 0) return;
    const preserved = messages.value.filter(
      (m) =>
        m.isSelf && (m.deliveryStatus === "sending" || m.deliveryStatus === "failed"),
    );
    void (async () => {
      await loadMessages(waddleId, channelId);
      if (
        preserved.length === 0 ||
        activeWaddleId.value !== waddleId ||
        activeChannelId.value !== channelId
      )
        return;
      const existingIds = new Set(messages.value.map((m) => m.id));
      const toAppend = preserved.filter((m) => !existingIds.has(m.id));
      if (toAppend.length > 0) messages.value = [...messages.value, ...toAppend];
    })();
  }

  async function loadMessages(waddleId: string, channelId: string) {
    if (!session.value) return;

    const requestId = ++messageRequestId;
    isLoadingMessages.value = true;
    clearActionError();

    try {
      // XEP-0313: Load message history via MAM (XMPP-native)
      const mamResults = xmppClient.value
        ? await xmppClient.value.queryMam(waddleId, channelId, 100)
        : [];

      if (
        requestId !== messageRequestId ||
        activeWaddleId.value !== waddleId ||
        activeChannelId.value !== channelId
      ) {
        return;
      }

      // Separate regular messages from timeline updates
      const regularMessages: LiveRoomMessage[] = [];
      const reactionUpdates: { targetId: string; nick: string; emojis: string[] }[] = [];
      const retractionUpdates: string[] = [];
      const correctionUpdates: { targetId: string; body: string; markup?: MarkupSpan[] }[] = [];

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
          regularMessages.push(msg);
        }
      }

      // Convert to timeline messages; accumulate in a map so replies that land
      // after their parent in the MAM batch can resolve a preview.
      const byId = new Map<string, TimelineMessage>();
      const timeline = regularMessages.map((m) => {
        const tm = fromLiveMessage(session.value!, m, (id) => byId.get(id));
        byId.set(tm.id, tm);
        return tm;
      });

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

      // Apply reactions from MAM history
      for (const update of reactionUpdates) {
        const target = timeline.find((m) => m.id === update.targetId);
        if (target) {
          const reactions: Record<string, string[]> = target.reactions ? { ...target.reactions } : {};
          for (const emoji of update.emojis) {
            if (!reactions[emoji]) reactions[emoji] = [];
            if (!reactions[emoji].includes(update.nick)) {
              reactions[emoji].push(update.nick);
            }
          }
          target.reactions = reactions;
        }
      }

      messages.value = timeline;
      if (requestId === messageRequestId) {
        isLoadingMessages.value = false;
      }
      await scrollToBottom();
    } catch (e) {
      if (requestId === messageRequestId) {
        actionError.value = normalizeError(e);
        isLoadingMessages.value = false;
      }
    }
  }

  /**
   * Backfill a thread via XEP-0313 MAM filtered by thread id. Returns every
   * archived reply whose `<thread>` element matches `threadId`. The thread
   * root does not carry `<thread>` (threads start when someone replies into
   * one), so MAM-by-thread never includes it — the panel resolves the root
   * separately from the loaded channel window.
   */
  async function backfillThread(threadId: string): Promise<void> {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!client || !waddleId || !channelId || !threadId || !session.value) return;
    const results = await client.queryMamByThread(waddleId, channelId, threadId, 100);
    if (
      xmppClient.value !== client ||
      activeWaddleId.value !== waddleId ||
      activeChannelId.value !== channelId
    ) {
      return;
    }
    const existingIds = new Set(messages.value.map((m) => m.id));
    const appended: TimelineMessage[] = [];
    const localById = new Map<string, TimelineMessage>(messages.value.map((m) => [m.id, m]));
    for (const raw of results) {
      if (existingIds.has(raw.id)) continue;
      const tm = fromLiveMessage(session.value, raw, (id) => localById.get(id));
      localById.set(tm.id, tm);
      appended.push(tm);
    }
    if (appended.length === 0) return;
    messages.value = [...messages.value, ...appended].sort((a, b) =>
      a.createdAt.localeCompare(b.createdAt),
    );
  }

  async function selectChannel(channelId: string) {
    if (!activeWaddleId.value) return;
    messages.value = [];
    clearTypingState();
    await loadMessages(activeWaddleId.value, channelId);
  }

  async function sendMessage(
    body?: string,
    markup?: MarkupSpan[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
    threadOverride?: { threadId: string; parentThreadId?: string },
  ) {
    const bodyText = body ?? draft.value;
    // markup !== undefined means this came from the rich editor (composer send)
    // undefined means it came from a programmatic send (GIF, etc.)
    const fromComposer = markup !== undefined;
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    const hasFiles = !!files && files.length > 0;
    if (!client || !waddleId || !channelId) return;
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
      // Thread membership is explicit: only set when the caller passed an
      // override (i.e. the thread panel composer). Bare `<reply>` from the
      // channel composer stays inline as a quote — not a thread join.
      const threadId = threadOverride?.threadId;
      const parentThreadId = threadOverride?.parentThreadId;
      const msgId = await client.sendGroupMessage(waddleId, channelId, bodyText, {
        markup,
        files: attachments,
        ...(wireReplyTo ? { replyTo: wireReplyTo } : {}),
        ...(threadId ? { threadId } : {}),
        ...(parentThreadId ? { parentThreadId } : {}),
      });
      const isStillCurrentChannel =
        xmppClient.value === client &&
        activeWaddleId.value === waddleId &&
        activeChannelId.value === channelId;

      if (msgId && session.value && isStillCurrentChannel) {
        // Optimistic insert: show message immediately with "sending" status
        const optimistic: TimelineMessage = {
          id: msgId,
          author: session.value.username,
          authorJid: `${currentRoomJid.value}/${session.value.username}`,
          body: bodyText.trim() || (attachments?.[0]?.url ?? ""),
          createdAt: new Date().toISOString(),
          isSelf: true,
          deliveryStatus: "sending",
          markup,
        };
        if (replyTo && parent) {
          optimistic.replyTo = {
            id: replyTo.id,
            author: replyTo.author,
            ...(replyTo.body ? { preview: replyTo.body } : {}),
          };
        }
        if (threadId) optimistic.threadId = threadId;
        if (parentThreadId) optimistic.parentThreadId = parentThreadId;
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
        pendingEchoClientIds.add(msgId);
        messages.value = [...messages.value, optimistic];
        void scrollToBottom();
      }

      // Clear draft on successful send (triggers ChatEditor clear via watcher)
      if (fromComposer && isStillCurrentChannel) {
        draft.value = "";
      }
      // Send "active" state after sending a message (stops composing indicator)
      if (isStillCurrentChannel) {
        if (composingTimeout) {
          clearTimeout(composingTimeout);
          composingTimeout = null;
        }
        lastChatState = "active";
      }
      await client.sendChatState(waddleId, channelId, "active");
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSending.value = false;
      uploadProgress.value = { uploading: false, progress: 0, filename: "" };
    }
  }

  async function toggleReaction(messageId: string, emoji: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value || !session.value)
      return;

    // Compute the new reaction set for this user
    const msg = messages.value.find((m) => m.id === messageId);
    const myNick = session.value.username;
    const currentReactions = msg?.reactions ?? {};

    // Gather all emojis this user currently has on this message
    const myEmojis = new Set<string>();
    for (const [e, nicks] of Object.entries(currentReactions)) {
      if (nicks.includes(myNick)) myEmojis.add(e);
    }

    // Toggle the emoji
    if (myEmojis.has(emoji)) {
      myEmojis.delete(emoji);
    } else {
      myEmojis.add(emoji);
    }

    try {
      await xmppClient.value.sendReaction(
        activeWaddleId.value,
        activeChannelId.value,
        messageId,
        [...myEmojis],
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;

    clearActionError();

    try {
      await xmppClient.value.sendRetraction(
        activeWaddleId.value,
        activeChannelId.value,
        messageId,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function moderateMessage(messageId: string, reason?: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;

    clearActionError();

    try {
      await xmppClient.value.sendModeration(
        activeWaddleId.value,
        activeChannelId.value,
        messageId,
        reason,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function editMessage(messageId: string, newBody: string, markup?: MarkupSpan[]) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value || !newBody.trim())
      return;

    clearActionError();

    try {
      await xmppClient.value.sendCorrection(
        activeWaddleId.value,
        activeChannelId.value,
        newBody,
        messageId,
        markup,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  function disconnect() {
    messageRequestId++;
    searchRequestId++;
    xmppStatus.value = { state: "offline", detail: "Live room offline" };
    clearTypingState();
    isLoadingMessages.value = false;
    isSearching.value = false;
  }

  function clearMessages() {
    messageRequestId++;
    messages.value = [];
    isLoadingMessages.value = false;
    clearTypingState();
  }

  async function searchMessages(query: string) {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!client || !waddleId || !channelId) return;
    const requestId = ++searchRequestId;
    const trimmed = query.trim();
    searchQuery.value = trimmed;
    if (!trimmed) {
      searchResults.value = [];
      isSearching.value = false;
      return;
    }
    isSearching.value = true;
    try {
      const results = await client.searchMessages(waddleId, channelId, trimmed);
      if (
        requestId === searchRequestId &&
        xmppClient.value === client &&
        activeWaddleId.value === waddleId &&
        activeChannelId.value === channelId
      ) {
        searchResults.value = results;
      }
    } catch {
      if (requestId === searchRequestId) {
        searchResults.value = [];
      }
    } finally {
      if (requestId === searchRequestId) {
        isSearching.value = false;
      }
    }
  }

  function clearSearch() {
    searchRequestId++;
    searchQuery.value = "";
    searchResults.value = [];
    isSearching.value = false;
  }

  function clearChannelActivity(roomJid: string) {
    const next = new Set(activeChannels.value);
    next.delete(roomJid);
    activeChannels.value = next;
  }

  return {
    xmppStatus,
    messages,
    draft,
    isLoadingMessages,
    isSending,
    timelineEl,
    currentRoomJid,
    typingUsers,
    roomHats,
    roomPresence,
    roomLastSeen,
    slowModeCooldown,
    loadMessages,
    backfillThread,
    selectChannel,
    sendMessage,
    uploadProgress,
    editMessage,
    retractMessage,
    moderateMessage,
    toggleReaction,
    markDisplayed,
    notifyComposing,
    disconnect,
    clearMessages,
    activeChannels,
    roomAvatarHashes,
    searchQuery,
    searchResults,
    isSearching,
    searchMessages,
    clearSearch,
    clearChannelActivity,
    scrollToBottom,
    lastMentionActivity,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
  };
}
