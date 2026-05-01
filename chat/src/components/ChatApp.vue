<script setup lang="ts">
import { type ComponentPublicInstance, computed, onMounted, onUnmounted, ref, watch, watchEffect } from "vue";
import { createKeystrok, type Keystrok } from "keystrok";
import { useWaddles } from "@/composables/useWaddles";
import { useMembers } from "@/composables/useMembers";
import { useDmConversations } from "@/composables/useDmConversations";
import { useDmMessaging } from "@/composables/useDmMessaging";
import { useMessaging } from "@/composables/useMessaging";
import { useThreads } from "@/composables/useThreads";
import { useUiState } from "@/composables/useUiState";
import { useAppUpdate } from "@/composables/useAppUpdate";
import { useNotifications } from "@/composables/useNotifications";
import { useChannelUnread } from "@/composables/useChannelUnread";
import { useTabUnreadIndicator } from "@/composables/useTabUnreadIndicator";
import { useWindowVisibility } from "@/composables/useWindowVisibility";
import { useReadReceipts } from "@/composables/useReadReceipts";
import { useVersion } from "@/composables/useVersion";
import { useRosterContacts } from "@/composables/useRosterContacts";
import { buildDmPath, buildPath, buildSettingsPath, parseRoute, pushDmRoute, pushRoute, pushSettingsRoute, resolveChannel, shouldLoadStructureForRoute } from "@/composables/useRouting";
import { barePeerJid, jidDomain, parseManagedRoomBareJid, roomBareJidFor } from "@/lib/xmpp-client";
import { mergeRoomHats, roomHatsFromMembers } from "@/lib/xmpp/occupant-badges";
import { connectionStore } from "@/lib/connection-store";
import LandingState from "@/components/chat/LandingState.vue";
import LoginScreen from "@/components/chat/LoginScreen.vue";
import WaddlesSidebar from "@/components/chat/WaddlesSidebar.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import HomeDashboard from "@/components/chat/HomeDashboard.vue";
import DmPanel from "@/components/chat/DmPanel.vue";
import ContentArea from "@/components/chat/ContentArea.vue";
import ThreadPanel from "@/components/chat/ThreadPanel.vue";
import { orderTimelineForScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { useScrollDirection } from "@/composables/useScrollDirection";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";
import UserSettingsPage from "@/components/chat/UserSettingsPage.vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import CreateChannelDialog from "@/components/modals/CreateChannelDialog.vue";
import WaddleSettingsDialog from "@/components/modals/WaddleSettingsDialog.vue";
import EditChannelDialog from "@/components/modals/EditChannelDialog.vue";
import NewDmDialog from "@/components/modals/NewDmDialog.vue";
import MemberManagement from "@/components/modals/MemberManagement.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import type { MemberSummary } from "@/lib/chat-types";
import type { ExtensionAnnotationAction, MarkupSpan, MessageReference, TimelineMessage } from "@/lib/chat-ui";
import { avatarLookupCandidates, mentionAutocompleteCandidates, mentionMatchesUsername, mergeMentionMembers } from "@/lib/mentions";
import { AI_CHATBOT_FEATURE, withAiAssistantMentionCandidate } from "@/lib/ai-thread-ux";
import {
  moveReactionSelection,
  preserveReactionSelection,
  quickReactionForKey,
  selectInitialReactionMessage,
  type ReactionModeScope,
} from "@/lib/reaction-mode";

const props = defineProps<{
  tenorApiKey?: string;
}>();

const tenorApiKey = props.tenorApiKey ?? "";

const ui = useUiState();
const { mode: scrollDirectionMode } = useScrollDirection();

const xmppClient = computed(() => connectionStore.client);
const session = computed(() => connectionStore.session);
const api = computed(() => connectionStore.api);

const waddles = useWaddles(
  api,
  xmppClient,
  session,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
);

const memberJidByNick = ref<Record<string, string>>({});
const mentionJidsByNickForSend = ref<Record<string, string>>({});

const messaging = useMessaging(
  session,
  api,
  xmppClient,
  waddles.activeSpaceId,
  waddles.activeChannelId,
  waddles.currentChannel,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
  mentionJidsByNickForSend,
);

const dmConversations = useDmConversations(
  session,
  xmppClient,
);

const channelUnread = useChannelUnread(xmppClient);
const rosterContacts = useRosterContacts(xmppClient);

const dmMessaging = useDmMessaging(
  session,
  xmppClient,
  dmConversations.activePeerJid,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
);

type ContentAreaHandle = ComponentPublicInstance & {
  messagesContainer: HTMLDivElement | null;
  scrollToPinnedEdge: (mode: ScrollDirectionMode) => Promise<boolean>;
};
const contentAreaRef = ref<ContentAreaHandle | null>(null);
const setContentAreaRef = (
  instance: ContentAreaHandle | null,
) => {
  contentAreaRef.value = instance;
};

watchEffect(() => {
  const timeline = contentAreaRef.value?.messagesContainer ?? null;
  const edgeScroller = contentAreaRef.value?.scrollToPinnedEdge ?? null;
  if (ui.sidebarMode.value === "dms") {
    dmMessaging.timelineEl.value = timeline;
    dmMessaging.timelineEdgeScroller.value = edgeScroller;
    messaging.timelineEl.value = null;
    messaging.timelineEdgeScroller.value = null;
  } else {
    messaging.timelineEl.value = timeline;
    messaging.timelineEdgeScroller.value = edgeScroller;
    dmMessaging.timelineEl.value = null;
    dmMessaging.timelineEdgeScroller.value = null;
  }
});

const activeMessages = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.messages.value : messaging.messages.value,
);
const activeFirstUnseenId = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.firstUnseenId.value : messaging.firstUnseenId.value,
);

// Thread panel state — stack = breadcrumb trail into nested sub-threads.
// Empty stack = panel closed. Only meaningful for channel messages since DMs
// don't use XEP-0201 threads yet.
const activeThreadStack = ref<string[]>([]);
const threads = useThreads(activeMessages);
type ReactionModeTarget = "main" | "thread";
const reactionModeTarget = ref<ReactionModeTarget | null>(null);
const reactionModeSelectedMessageId = ref<string | null>(null);
const CHAT_KEYSTROK_SCOPE = "chat";
const REACTION_MODE_KEYSTROK_SCOPE = "chat-reaction-mode";
let chatKeystrok: Keystrok | null = null;

function getThreadLabel(threadId: string): string {
  const entry = threads.resolveEntry(threadId);
  const body = entry?.root?.body?.trim() ?? "";
  return body.length > 0 ? body.slice(0, 40) : threadId.slice(0, 8);
}

const orderedMainReactionMessages = computed(() =>
  orderTimelineForScrollDirection(
    reactionModeMessageCandidates(activeMessages.value.filter((message) => !message.threadId || message.id === message.threadId)),
    scrollDirectionMode.value,
  ),
);

const activeThreadReactionMessages = computed<TimelineMessage[]>(() => {
  if (ui.sidebarMode.value !== "channels") return [];
  const threadId = activeThreadStack.value[activeThreadStack.value.length - 1];
  if (!threadId) return [];
  const entry = threads.resolveEntry(threadId);
  if (!entry) return [];
  const orderedChildren = orderTimelineForScrollDirection(reactionModeMessageCandidates(entry.directChildren), scrollDirectionMode.value);
  return entry.root ? reactionModeMessageCandidates([entry.root, ...orderedChildren]) : orderedChildren;
});

const reactionModeMessages = computed(() =>
  reactionModeTarget.value === "thread"
    ? activeThreadReactionMessages.value
    : orderedMainReactionMessages.value,
);

const reactionModeScope = computed<ReactionModeScope>(() =>
  reactionModeTarget.value === "thread" ? "thread" : "feed",
);

const reactionModeState = computed(() =>
  reactionModeTarget.value
    ? {
        selectedMessageId: reactionModeSelectedMessageId.value,
      }
    : null,
);

function activeReactionTarget(): ReactionModeTarget | null {
  if (ui.activePage.value !== "chat") return null;
  if (ui.sidebarMode.value === "channels" && activeThreadStack.value.length > 0) return "thread";
  if (ui.sidebarMode.value === "dms" && activeDmPeer.value) return "main";
  if (ui.sidebarMode.value === "channels" && waddles.currentChannel.value) return "main";
  return null;
}

function reactionModeMessageCandidates(messages: readonly TimelineMessage[]): TimelineMessage[] {
  return messages.map((message) => ({
    ...message,
    canReact: ui.sidebarMode.value === "dms" || !!message.reactionTargetId,
  }));
}

function exitReactionMode() {
  reactionModeTarget.value = null;
  reactionModeSelectedMessageId.value = null;
  chatKeystrok?.scope(REACTION_MODE_KEYSTROK_SCOPE).deactivate();
}

function startReactionMode(target = activeReactionTarget()): boolean {
  if (!target) return false;
  const messages = target === "thread" ? activeThreadReactionMessages.value : orderedMainReactionMessages.value;
  const scope = target === "thread" ? "thread" : "feed";
  const selected = selectInitialReactionMessage(messages, scope);
  if (!selected) return false;
  reactionModeTarget.value = target;
  reactionModeSelectedMessageId.value = selected;
  chatKeystrok?.scope(REACTION_MODE_KEYSTROK_SCOPE).override();
  return true;
}

function reactToSelectedMessage(emoji: string) {
  const messageId = reactionModeSelectedMessageId.value;
  if (!messageId) return;
  reactActiveMessage(messageId, emoji);
  exitReactionMode();
}

function isEditableKeyTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.closest("[contenteditable='true']")) return true;
  return !!target.closest("input, textarea, select");
}

function isComposerEditorTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && !!target.closest(".chat-composer .ProseMirror");
}

function isComposerEditorEmpty(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const editor = target.closest(".chat-composer .ProseMirror");
  if (!(editor instanceof HTMLElement)) return false;
  return (editor.textContent ?? "").length === 0;
}

function canStartReactionModeFromEvent(event: KeyboardEvent): boolean {
  if (anyModalOpen()) return false;
  if (!isEditableKeyTarget(event.target)) return true;
  if (!isComposerEditorTarget(event.target)) return false;
  return isComposerEditorEmpty(event.target);
}

function consumeKeystrokEvent(event: KeyboardEvent) {
  event.preventDefault();
  event.stopPropagation();
}

function handleStartReactionModeShortcut(event: KeyboardEvent) {
  if (!canStartReactionModeFromEvent(event)) return;
  if (startReactionMode()) {
    consumeKeystrokEvent(event);
  }
}

function handleLiteralPlusKeyDown(event: KeyboardEvent) {
  if (event.key !== "+" || event.ctrlKey || event.metaKey || event.altKey) return;
  if (reactionModeTarget.value) {
    if (!ensureReactionModeOwnsEvent(event)) return;
    consumeKeystrokEvent(event);
    return;
  }
  handleStartReactionModeShortcut(event);
}

function shouldYieldReactionModeForEvent(event: KeyboardEvent): boolean {
  if (anyModalOpen()) return true;
  return isEditableKeyTarget(event.target);
}

function ensureReactionModeOwnsEvent(event: KeyboardEvent): boolean {
  if (!shouldYieldReactionModeForEvent(event)) return true;
  exitReactionMode();
  return false;
}

function handleReactionModeMove(event: KeyboardEvent, direction: "previous" | "next") {
  if (!ensureReactionModeOwnsEvent(event)) return;
  reactionModeSelectedMessageId.value = moveReactionSelection(
    reactionModeSelectedMessageId.value,
    reactionModeMessages.value,
    reactionModeScope.value,
    direction,
  );
  consumeKeystrokEvent(event);
}

function handleReactionModeQuickReaction(event: KeyboardEvent) {
  if (!ensureReactionModeOwnsEvent(event)) return;
  const emoji = quickReactionForKey(event.key);
  if (emoji) reactToSelectedMessage(emoji);
  consumeKeystrokEvent(event);
}

function handleReactionModeEscape(event: KeyboardEvent) {
  const shouldYield = shouldYieldReactionModeForEvent(event);
  exitReactionMode();
  if (!shouldYield) consumeKeystrokEvent(event);
}
const activeDraft = computed({
  get: () => (ui.sidebarMode.value === "dms" ? dmMessaging.draft.value : messaging.draft.value),
  set: (value: string) => {
    if (ui.sidebarMode.value === "dms") dmMessaging.draft.value = value;
    else messaging.draft.value = value;
  },
});
const activeForumTitle = computed({
  get: () => (ui.sidebarMode.value === "dms" ? "" : messaging.forumPostTitle.value),
  set: (value: string) => {
    if (ui.sidebarMode.value !== "dms") {
      messaging.forumPostTitle.value = value;
    }
  },
});
const activeTypingUsers = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.typingUsers.value : messaging.typingUsers.value,
);
const activeIsLoadingMessages = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.isLoadingMessages.value : messaging.isLoadingMessages.value,
);
const activeIsLoadingOlderMessages = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.isLoadingOlderMessages.value : messaging.isLoadingOlderMessages.value,
);
const activeHasOlderMessages = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.hasOlderMessages.value : messaging.hasOlderMessages.value,
);
const activeIsSending = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.isSending.value : messaging.isSending.value,
);
const activeSearchResults = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.searchResults.value : messaging.searchResults.value,
);
const activeIsSearching = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.isSearching.value : messaging.isSearching.value,
);

const selfDomain = computed(() => (session.value ? jidDomain(session.value.jid) : ""));
const members = useMembers(
  xmppClient,
  waddles.activeSpaceId,
  waddles.activeChannelId,
  waddles.members,
  messaging.roomPresence,
  memberJidByNick,
  waddles.canManageMembers,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
  waddles.loadStructure,
);
const mergedMentionMembers = computed(() =>
  mergeMentionMembers({
    members: waddles.members.value,
    roomPresence: messaging.roomPresence.value,
    memberJidsByNick: memberJidByNick.value,
  }),
);
const authorJidByNick = computed(() => mergedMentionMembers.value.authorJidByNick);
watch(authorJidByNick, (value) => {
  mentionJidsByNickForSend.value = value;
}, { immediate: true });
const fetchedAvatarUrlByJid = ref<Record<string, string | null>>({});
const avatarFetchStateByJid = ref<Record<string, "pending" | "done">>({});
const aiAssistantEnabled = computed(() =>
  waddles.currentChannel.value?.features?.includes(AI_CHATBOT_FEATURE) === true,
);
const mentionCandidates = computed(() =>
  withAiAssistantMentionCandidate(
    mentionAutocompleteCandidates(mergedMentionMembers.value.members),
    aiAssistantEnabled.value,
  ).map((candidate) => {
    if (candidate.kind === "broadcast" || !candidate.jid) return candidate;
    return {
      ...candidate,
      avatar_url: fetchedAvatarUrlByJid.value[candidate.jid] ?? candidate.avatar_url,
    };
  }),
);
const mentionSourceDiagnostic = computed(() =>
  mergedMentionMembers.value.diagnostics.join(" "),
);
const activeDmPeer = computed(() => {
  const active = dmConversations.activePeerJid.value;
  if (!active) return null;
  const conversation = dmConversations.conversations.value.find((c) => c.peerJid === active);
  if (!conversation) return null;
  return {
    peerJid: conversation.peerJid,
    peerUsername: conversation.peerUsername,
    presenceShow: conversation.presenceShow,
  };
});

const computedChannelUnreadMap = computed(() => channelUnread.channelUnreadMap());
// Thread rows are drill-down detail; room unread already carries the
// server-equivalent conversation total, so the browser badge must not add both.
const totalTabUnreadCount = computed(() =>
  connectionStore.appState === "ready" && session.value && xmppClient.value
    ? dmConversations.totalUnreadCount.value + channelUnread.totalUnreadCount.value
    : 0
);
useTabUnreadIndicator(totalTabUnreadCount, {
  shouldAcceptServiceWorkerUnreadCount: () =>
    connectionStore.appState === "ready" && !!session.value && !!xmppClient.value,
  onServiceWorkerUnreadCount: () => Promise.all([
    dmConversations.hydrateFromInbox(),
    channelUnread.hydrateFromInbox(),
  ]).then((results) => results.every(Boolean)),
});

const { isWindowFocused } = useWindowVisibility();
const readReceiptsKind = computed<"channel" | "dm" | null>(() => {
  if (ui.sidebarMode.value === "dms") {
    return dmConversations.activePeerJid.value ? "dm" : null;
  }
  return waddles.activeChannelId.value ? "channel" : null;
});
const readReceiptsActiveRoomJid = computed<string | null>(() => {
  if (readReceiptsKind.value !== "channel") return null;
  const channelId = waddles.activeChannelId.value;
  const sess = session.value;
  if (!channelId || !sess) return null;
  return roomBareJidFor(sess, channelId);
});
const readReceiptsActivePeerJid = computed<string | null>(() =>
  readReceiptsKind.value === "dm" ? dmConversations.activePeerJid.value : null,
);
const readReceiptsIsPinnedAtEdge = computed<boolean>(() =>
  readReceiptsKind.value === "dm"
    ? dmMessaging.isPinnedAtEdge.value
    : messaging.isPinnedAtEdge.value,
);
const readReceiptsLatestRemoteId = computed<string | null>(() =>
  readReceiptsKind.value === "dm"
    ? dmMessaging.latestRemoteMessageId.value
    : messaging.latestRemoteMessageId.value,
);
const readReceiptsUnreadCount = computed<number>(() => {
  if (readReceiptsKind.value === "dm") {
    const peer = readReceiptsActivePeerJid.value;
    if (!peer) return 0;
    return dmConversations.conversations.value.find((c) => c.peerJid === peer)?.unreadCount ?? 0;
  }
  if (readReceiptsKind.value === "channel") {
    const channelId = waddles.activeChannelId.value;
    if (!channelId) return 0;
    return computedChannelUnreadMap.value[channelId]?.unread ?? 0;
  }
  return 0;
});
useReadReceipts({
  isWindowFocused,
  isPinnedAtEdge: readReceiptsIsPinnedAtEdge,
  activeKind: readReceiptsKind,
  activeRoomJid: readReceiptsActiveRoomJid,
  activePeerJid: readReceiptsActivePeerJid,
  latestRemoteMessageId: readReceiptsLatestRemoteId,
  unreadCountForActive: readReceiptsUnreadCount,
  markChannelRead: (jid) => channelUnread.markRead(jid),
  markDmRead: (peer) => dmConversations.markRead(peer),
  markDisplayed: (id) => activeTarget.value.markDisplayed(id),
});

const notifications = useNotifications();
const appUpdate = useAppUpdate();
const version = useVersion(xmppClient);
const avatarCandidates = computed(() =>
  avatarLookupCandidates({
    members: mergedMentionMembers.value.members,
    messages: messaging.messages.value,
    authorJidByNick: authorJidByNick.value,
    selfDomain: selfDomain.value,
  }),
);
const avatarUrlByAuthor = computed<Record<string, string | null>>(() => {
  const avatars: Record<string, string | null> = {};

  if (session.value) {
    avatars[session.value.username] = session.value.avatar_url;
  }

  for (const member of waddles.members.value) {
    const fetched = fetchedAvatarUrlByJid.value[member.jid];
    const avatar = fetched ?? member.avatar_url;
    if (!(member.username in avatars) || avatar) {
      avatars[member.username] = avatar;
    }
  }

  for (const member of mergedMentionMembers.value.members) {
    const fetched = fetchedAvatarUrlByJid.value[member.jid];
    const avatar = fetched ?? member.avatar_url;
    if (!(member.username in avatars) || avatar) {
      avatars[member.username] = avatar;
    }
  }

  for (const candidate of avatarCandidates.value) {
    const fetched = fetchedAvatarUrlByJid.value[candidate.jid];
    const avatar = fetched ?? candidate.avatar_url;
    if (!(candidate.nick in avatars) || avatar) {
      avatars[candidate.nick] = avatar;
    }
  }

  return avatars;
});
const membersWithAvatars = computed<MemberSummary[]>(() =>
  waddles.sortedMembers.value.map((member) => ({
    ...member,
    avatar_url: fetchedAvatarUrlByJid.value[member.jid] ?? member.avatar_url,
  })),
);
const authorHatsByNick = computed(() =>
  mergeRoomHats(roomHatsFromMembers(waddles.members.value), messaging.roomHats.value),
);

watch(
  () => [xmppClient.value, avatarCandidates.value.map((candidate) => candidate.jid).join("\n")] as const,
  ([client]) => {
    if (!client) return;
    for (const candidate of avatarCandidates.value) {
      const jid = candidate.jid;
      if (!jid || candidate.avatar_url || avatarFetchStateByJid.value[jid]) continue;
      avatarFetchStateByJid.value = { ...avatarFetchStateByJid.value, [jid]: "pending" };
      void client.fetchUserAvatar(jid)
        .then((avatarUrl) => {
          fetchedAvatarUrlByJid.value = { ...fetchedAvatarUrlByJid.value, [jid]: avatarUrl };
        })
        .catch(() => {
          fetchedAvatarUrlByJid.value = { ...fetchedAvatarUrlByJid.value, [jid]: null };
        })
        .finally(() => {
          avatarFetchStateByJid.value = { ...avatarFetchStateByJid.value, [jid]: "done" };
        });
    }
  },
  { immediate: true },
);

function resolveChannelNameFromJid(roomJid: string): string | null {
  const managedRoom = parseManagedRoomBareJid(roomJid);
  if (!managedRoom) return null;
  return waddles.channels.value.find((c) => c.id === managedRoom.channelId)?.name ?? null;
}

watch(() => messaging.lastMentionActivity.value, (event) => {
  if (!event) return;

  const channelName = resolveChannelNameFromJid(event.roomJid) ?? "unknown";
  const isBroadcast = !!event.broadcastMention;
  const isPersonalMention = event.mentions?.some((mention) =>
    mentionMatchesUsername(mention, connectionStore.session?.username)
  );

  if (isBroadcast || isPersonalMention) {
    notifications.showMentionNotification({
      senderNick: event.nick,
      channelName,
      body: event.body,
      roomJid: event.roomJid,
      isBroadcast,
      onNavigate: (roomJid) => {
        const managedRoom = parseManagedRoomBareJid(roomJid);
        if (!managedRoom) return;
        void selectChannel(managedRoom.channelId);
      },
    });
  }

  messaging.lastMentionActivity.value = null;
});

watch(xmppClient, (client) => {
  if (!client || !session.value) return;
  client.setDirectMessageHandler((msg) => {
    dmMessaging.onIncomingMessage(msg);
    dmConversations.receiveIncomingDm(msg);
    const isSelf = barePeerJid(msg.fromJid) === barePeerJid(session.value?.jid ?? "");
    const isViewingThisDm = ui.sidebarMode.value === "dms"
      && dmConversations.activePeerJid.value === msg.peerJid;
    if (!isSelf && !isViewingThisDm) {
      notifications.showDmNotification({
        senderUsername: msg.nick,
        peerJid: msg.peerJid,
        body: msg.body,
        onNavigate: (peerJid) => {
          void handleOpenDm(peerJid);
        },
      });
    }
  });
  client.setDmChatStateHandler(dmMessaging.onChatState);
  client.setDmDisplayedHandler(dmMessaging.onDisplayed);
  client.setDmReactionHandler(dmMessaging.onReaction);
  client.setPresenceUpdateHandler((event) => {
    dmConversations.updatePresence(event);
    rosterContacts.updatePresence(event.bareJid, event.show);
  });
  client.setMemberJidHandler((nick, bareJid) => {
    memberJidByNick.value = { ...memberJidByNick.value, [nick]: bareJid };
  });
  // XEP-0198 fan-out: the same message ID only ever appears in one timeline
  // (rooms vs DMs), so calling both is idempotent — whichever owns the id
  // updates it, the other no-ops.
  client.setMessageAckHandler((id) => {
    messaging.onMessageAck(id);
    dmMessaging.onMessageAck(id);
  });
  client.setMessageDeliveryFailureHandler((id) => {
    messaging.onMessageDeliveryFailure(id);
    dmMessaging.onMessageDeliveryFailure(id);
  });
  client.setQueuedMessageStatusHandler((id, status) => {
    messaging.onMessageQueueStatus(id, status);
    dmMessaging.onMessageQueueStatus(id, status);
  });
  client.setInboxPushHandler((entry) => {
    dmConversations.onInboxPush(entry);
    channelUnread.onInboxPush(entry);
  });
  client.setSessionLifecycleHandler((event) => {
    messaging.onSessionLifecycle(event);
    dmMessaging.onSessionLifecycle(event);
    void dmConversations.hydrateFromInbox();
    void channelUnread.hydrateFromInbox();
  });
}, { immediate: true });

const isApplyingRoute = ref(false);
let routeRequestId = 0;
const settingsPath = buildSettingsPath();

const currentChatPath = computed(() =>
  ui.sidebarMode.value === "dms" && activeDmPeer.value
    ? buildDmPath(activeDmPeer.value.peerUsername)
    : buildPath(waddles.currentChannel.value, activeThreadStack.value),
);

async function setupPushSubscription() {
  if (!xmppClient.value || !connectionStore.session) return;
  await notifications.syncPushSubscription(xmppClient.value, connectionStore.session.jid);
}

async function handleRequestNotifications() {
  const state = await notifications.requestPermission();
  if (state === "granted") {
    await setupPushSubscription();
  }
}

async function handleToggleNotifications() {
  notifications.notificationsEnabled.value = !notifications.notificationsEnabled.value;
  if (notifications.notificationsEnabled.value) {
    await setupPushSubscription();
  } else if (xmppClient.value && connectionStore.session) {
    await notifications.disablePushSubscription(xmppClient.value, connectionStore.session.jid);
  }
}

async function refreshAppUpdate() {
  await appUpdate.applyUpdate();
}

const activeTarget = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging : messaging,
);

const activeUploadProgress = computed(() =>
  ui.sidebarMode.value === "dms" ? dmMessaging.uploadProgress.value : messaging.uploadProgress.value,
);
const activeActionError = computed(() =>
  ui.actionError.value || (ui.sidebarMode.value === "channels" ? mentionSourceDiagnostic.value : "")
);
let setupPromptShown = false;

function showFirstRunSetupIfNeeded() {
  if (
    setupPromptShown ||
    connectionStore.appState !== "ready" ||
    ui.sidebarMode.value !== "channels" ||
    ui.activePage.value !== "chat" ||
    !waddles.canManageChannels.value ||
    !waddles.isEmptyDeployment.value ||
    waddles.isLoadingStructure.value
  ) {
    return;
  }

  setupPromptShown = true;
  ui.createChannelContextSpaceId.value = null;
  waddles.createChannelForm.value = {
    intent: "space-with-muc",
    space_name: "",
    space_description: "",
    muc_name: "general",
    muc_description: "",
    muc_type: "text",
  };
  ui.showCreateChannel.value = true;
}

function openCreateChannelDialog(spaceId: string | null = null) {
  ui.createChannelContextSpaceId.value = spaceId;
  waddles.prepareCreateChannelForContext(spaceId);
  ui.showCreateChannel.value = true;
}

async function sendActiveMessage(
  body?: string,
  markup?: MarkupSpan[],
  references?: MessageReference[],
  files?: Array<File | Blob>,
  replyTo?: { id: string; author: string; body?: string },
  forumTitle?: string,
) {
  if (ui.sidebarMode.value === "dms") {
    await dmMessaging.sendMessage(body, markup, references, files, replyTo);
    return;
  }
  await messaging.sendMessage(body, markup, references, files, replyTo, forumTitle);
}

async function sendThreadMessage(
  body: string,
  markup: MarkupSpan[],
  references: MessageReference[],
  files: Array<File | Blob> | undefined,
  replyTo: { id: string; author: string; body?: string } | undefined,
  threadOverride: { threadId: string; parentThreadId?: string },
) {
  await messaging.sendMessage(body, markup, references, files, replyTo, threadOverride);
}

function sendGif(url: string) {
  void sendActiveMessage(url, [], []);
}

function openThread(threadId: string) {
  if (!threadId) return;
  if (
    activeThreadStack.value.length > 0 &&
    activeThreadStack.value[activeThreadStack.value.length - 1] === threadId
  ) {
    return;
  }
  activeThreadStack.value = [threadId];
  void messaging.backfillThread(threadId);
}

async function onSelectThread(channelId: string, threadId: string) {
  // Navigate to the channel if not already there
  if (waddles.activeChannelId.value !== channelId) {
    await selectChannel(channelId);
  }
  // Mark thread as read
  if (connectionStore.session) {
    const roomJid = roomBareJidFor(connectionStore.session, channelId);
    channelUnread.markThreadRead(roomJid, threadId);
  }
  openThread(threadId);
}

function pushThread(threadId: string) {
  if (!threadId) return;
  if (
    activeThreadStack.value.length > 0 &&
    activeThreadStack.value[activeThreadStack.value.length - 1] === threadId
  ) {
    return;
  }
  activeThreadStack.value = [...activeThreadStack.value, threadId];
  void messaging.backfillThread(threadId);
}

function popThreadTo(index: number) {
  if (index < 0) {
    activeThreadStack.value = [];
    return;
  }
  activeThreadStack.value = activeThreadStack.value.slice(0, index + 1);
}

function closeThreadPanel() {
  activeThreadStack.value = [];
}

function notifyActiveComposing() {
  activeTarget.value.notifyComposing();
}

function editActiveMessage(messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]) {
  if (ui.sidebarMode.value === "dms") {
    void dmMessaging.editMessage(messageId, newBody, markup, references);
    return;
  }
  void messaging.editMessage(messageId, newBody, markup, references);
}

function retractActiveMessage(messageId: string) {
  void activeTarget.value.retractMessage(messageId);
}

function reactActiveMessage(messageId: string, emoji: string) {
  void activeTarget.value.toggleReaction(messageId, emoji);
}

function markActiveDisplayed(messageId: string) {
  activeTarget.value.markDisplayed(messageId);
}

async function invokeActiveExtensionAction(action: ExtensionAnnotationAction) {
  await activeTarget.value.invokeExtensionAction(action);
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

function loadOlderThreadMessages(threadId: string) {
  void messaging.loadOlderThreadMessages(threadId);
}

function anyModalOpen(): boolean {
  const domModalOpen = typeof document !== "undefined" && !!document.querySelector("[aria-modal='true']");
  return ui.showCreateChannel.value ||
    ui.showEditChannel.value ||
    ui.showWaddleSettings.value ||
    ui.showMembers.value ||
    ui.confirmDeleteWaddle.value ||
    ui.confirmDeleteChannel.value ||
    ui.showNewDm.value ||
    ui.confirmRemoveMember.value !== null ||
    ui.showMobileNav.value ||
    ui.showMobileDetails.value ||
    domModalOpen;
}

function handleChatEscape(event: KeyboardEvent) {
  if (activeThreadStack.value.length === 0) return;
  // Don't intercept Escape when any dialog/drawer is open so they can close first.
  if (anyModalOpen()) return;
  activeThreadStack.value = activeThreadStack.value.slice(0, -1);
  consumeKeystrokEvent(event);
}

function bindChatKeystrokShortcuts() {
  chatKeystrok = createKeystrok();
  chatKeystrok
    .bind("escape", handleChatEscape, { scope: CHAT_KEYSTROK_SCOPE })
    .bind("escape", handleReactionModeEscape, { scope: REACTION_MODE_KEYSTROK_SCOPE })
    .bind("up", (event) => handleReactionModeMove(event, "previous"), { scope: REACTION_MODE_KEYSTROK_SCOPE })
    .bind("down", (event) => handleReactionModeMove(event, "next"), { scope: REACTION_MODE_KEYSTROK_SCOPE })
    .bind("backspace", handleReactionModeEscape, { scope: REACTION_MODE_KEYSTROK_SCOPE })
    .bind("delete", handleReactionModeEscape, { scope: REACTION_MODE_KEYSTROK_SCOPE });

  for (const key of ["1", "2", "3", "4", "5"]) {
    chatKeystrok.bind(key, handleReactionModeQuickReaction, { scope: REACTION_MODE_KEYSTROK_SCOPE });
  }

  chatKeystrok.scope(CHAT_KEYSTROK_SCOPE).activate();
}

// --- Deep linking ---

function updateUrl() {
  if (isApplyingRoute.value) return;
  if (ui.activePage.value === "settings") {
    pushSettingsRoute("app");
    return;
  }
  if (ui.activePage.value === "dashboard") {
    pushRoute(null);
    return;
  }
  if (ui.sidebarMode.value === "dms" && activeDmPeer.value) {
    pushDmRoute(activeDmPeer.value.peerUsername);
  } else {
    pushRoute(waddles.currentChannel.value, activeThreadStack.value);
  }
}

watch(
  [waddles.activeChannelId, ui.sidebarMode, () => dmConversations.activePeerJid.value],
  () => {
    // Channel / DM / mode changes close any open thread panel — the ids inside
    // the stack belong to the channel we just left.
    activeThreadStack.value = [];
    exitReactionMode();
    updateUrl();
  },
);

watch(activeThreadStack, () => {
  exitReactionMode();
  updateUrl();
}, { deep: true });
watch(() => ui.activePage.value, () => {
  exitReactionMode();
  updateUrl();
});

watch(
  [reactionModeMessages, reactionModeScope],
  () => {
    if (!reactionModeTarget.value) return;
    const selected = preserveReactionSelection(
      reactionModeSelectedMessageId.value,
      reactionModeMessages.value,
      reactionModeScope.value,
    );
    if (!selected) {
      exitReactionMode();
      return;
    }
    reactionModeSelectedMessageId.value = selected;
  },
  { flush: "post" },
);

function openUserSettings() {
  ui.showMobileNav.value = false;
  ui.showMobileDetails.value = false;
  ui.activePage.value = "settings";
}

function closeUserSettings() {
  const state = window.history.state as { waddlePage?: string; origin?: string } | null;
  if (
    window.location.pathname === settingsPath
    && state?.waddlePage === "settings"
    && state.origin === "app"
  ) {
    window.history.back();
    return;
  }
  ui.activePage.value = waddles.currentChannel.value || activeDmPeer.value ? "chat" : "dashboard";
  if (window.location.pathname + window.location.search !== currentChatPath.value) {
    if (ui.sidebarMode.value === "dms" && activeDmPeer.value) {
      pushDmRoute(activeDmPeer.value.peerUsername);
    } else {
      pushRoute(waddles.currentChannel.value, activeThreadStack.value);
    }
  }
}

function onPopState() {
  const route = parseRoute(window.location.pathname, window.location.search);
  ui.activePage.value = route.page;
  if (route.page === "settings") {
    activeThreadStack.value = [];
    return;
  }
  if (route.page === "dashboard") {
    ui.sidebarMode.value = "channels";
    dmConversations.closeDm();
    waddles.activeChannelId.value = null;
    messaging.clearMessages();
    activeThreadStack.value = [];
    return;
  }
  if (!route.channelSlug && !route.dmUsername) return;

  const requestId = ++routeRequestId;
  isApplyingRoute.value = true;
  void applyRouteTarget(route, requestId).finally(() => {
    if (requestId === routeRequestId) {
      isApplyingRoute.value = false;
    }
  });
}

async function applyRouteTarget(route: ReturnType<typeof parseRoute>, requestId: number) {
  ui.activePage.value = route.page;
  if (route.page === "settings") {
    activeThreadStack.value = [];
    return;
  }
  if (route.page === "dashboard") {
    ui.sidebarMode.value = "channels";
    dmConversations.closeDm();
    waddles.activeChannelId.value = null;
    messaging.clearMessages();
    activeThreadStack.value = [];
    if (shouldLoadStructureForRoute(route, waddles.channels.value.length)) {
      await waddles.loadStructure(null, { noChannelSelect: true });
    }
    return;
  }
  if (route.dmUsername) {
    const username = route.dmUsername.replace(/^@/, "").trim();
    if (username) {
      const domain = session.value ? jidDomain(session.value.jid) : "";
      if (!domain) return;
      await handleOpenDm(`${username}@${domain}`);
    }
    return;
  }

  if (shouldLoadStructureForRoute(route, waddles.channels.value.length)) {
    await waddles.loadStructure();
    if (requestId !== routeRequestId) return;
  }

  if (route.channelSlug) {
    const ch = resolveChannel(route.channelSlug, waddles.channels.value);
    if (!ch) {
      waddles.activeChannelId.value = null;
      messaging.clearMessages();
      return;
    }
    waddles.activeChannelId.value = ch.id;
    void waddles.reloadChannelMembers(ch.id);
    messaging.clearMessages();
    await messaging.loadMessages(ch.spaceId ?? "", ch.id);
  } else if (waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", waddles.activeChannelId.value);
  }

  // Restore the thread panel from the URL and initialize paging for every
  // visible thread pane. Dedupe in the messaging composable keeps already
  // loaded roots/replies stable.
  activeThreadStack.value = route.threadStack;
  for (const threadId of route.threadStack) {
    void messaging.backfillThread(threadId);
  }
}

// --- Bootstrap (watches connection store) ---

async function onConnectionReady() {
  const route = parseRoute(window.location.pathname, window.location.search);
  const requestId = ++routeRequestId;
  isApplyingRoute.value = true;

  try {
    if (route.page === "dashboard") {
      await waddles.loadStructure(null, { noChannelSelect: true });
    } else {
      await waddles.loadSpace({ loadStructure: !shouldLoadStructureForRoute(route, waddles.channels.value.length) });
    }
    if (requestId === routeRequestId) {
      await applyRouteTarget(route, requestId);
    }
    showFirstRunSetupIfNeeded();
  } finally {
    if (requestId === routeRequestId) {
      isApplyingRoute.value = false;
      updateUrl();
    }
  }

  void dmConversations.hydrateFromInbox();
  void channelUnread.hydrateFromInbox();
  void rosterContacts.loadRosterContacts();

  // Register service worker and sync push subscription (best-effort, non-blocking)
  void (async () => {
    await notifications.registerServiceWorker();
    await setupPushSubscription();
  })();
}

// --- Actions ---

async function handleLogout() {
  ui.activePage.value = "dashboard";
  messaging.disconnect();
  dmMessaging.disconnect();
  waddles.clearData();
  channelUnread.clearAll();
  rosterContacts.clearRosterContacts();
  setupPromptShown = false;
  messaging.clearMessages();
  dmMessaging.clearMessages();
  pushRoute(null);
  await connectionStore.logout();
}

async function selectChannel(channelId: string) {
  ui.activePage.value = "chat";
  ui.sidebarMode.value = "channels";
  dmConversations.closeDm();
  memberJidByNick.value = {};
  waddles.activeChannelId.value = channelId;
  void waddles.reloadChannelMembers(channelId);
  messaging.clearMessages();
  // XEP-0502: Clear activity indicator for this channel
  if (connectionStore.session) {
    const roomJid = roomBareJidFor(connectionStore.session, channelId);
    messaging.clearChannelActivity(roomJid);
  }
  const unreadAtLoad = computedChannelUnreadMap.value[channelId]?.unread ?? 0;
  await messaging.loadMessages(
    waddles.currentChannel.value?.spaceId ?? "",
    channelId,
    unreadAtLoad,
  );
  ui.showMobileNav.value = false;
}

async function handleOpenDm(peerJid: string) {
  ui.activePage.value = "chat";
  ui.sidebarMode.value = "dms";
  await dmConversations.openDm(peerJid);
  dmMessaging.clearMessages();
  const activePeer = dmConversations.activePeerJid.value;
  if (activePeer) {
    const unreadAtLoad = dmConversations.conversations.value.find((c) => c.peerJid === activePeer)?.unreadCount ?? 0;
    await dmMessaging.loadMessages(activePeer, unreadAtLoad);
  }
  ui.showMobileNav.value = false;
}

async function selectDm(peerJid: string) {
  await handleOpenDm(peerJid);
}

async function handleNewDm(username: string) {
  if (!selfDomain.value) return;
  await handleOpenDm(`${username}@${selfDomain.value}`);
}

async function handleCreateChannel() {
  const created = await waddles.createChannel();
  if (!created) return;
  ui.showCreateChannel.value = false;
  ui.createChannelContextSpaceId.value = null;
  if (created.intent === "space") {
    // Space-only: topology reloaded, no room to load — leave channel unselected.
    return;
  }
  // MUC was created (muc / space-muc / space-with-muc): select and load messages.
  ui.activePage.value = "chat";
  messaging.clearMessages();
  await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", created.channelId);
}

async function handleUpdateChannel() {
  await waddles.updateChannel();
  ui.showEditChannel.value = false;
}

async function handleDeleteChannel() {
  ui.confirmDeleteChannel.value = true;
}

async function confirmDeleteChannel() {
  ui.confirmDeleteChannel.value = false;
  await waddles.deleteChannel();
  ui.showEditChannel.value = false;
  if (waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", waddles.activeChannelId.value);
  }
}

async function handleUpdateWaddle() {
  await waddles.updateWaddle();
  ui.showWaddleSettings.value = false;
}

async function handleDeleteWaddle() {
  ui.confirmDeleteWaddle.value = true;
}

async function confirmDeleteWaddle() {
  ui.confirmDeleteWaddle.value = false;
  await waddles.deleteWaddle();
  ui.showWaddleSettings.value = false;
  if (waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", waddles.activeChannelId.value);
  }
}

let pendingRemoveMember: MemberSummary | null = null;

function handleRemoveMember(member: MemberSummary) {
  pendingRemoveMember = member;
  ui.confirmRemoveMember.value = member.username;
}

async function confirmRemoveMember() {
  if (pendingRemoveMember) {
    const member = pendingRemoveMember;
    pendingRemoveMember = null;
    ui.confirmRemoveMember.value = null;
    await members.removeMember(member);
  }
}

function openChannelEdit() {
  if (waddles.currentChannel.value) {
    ui.showEditChannel.value = true;
  }
}

// Watch for connection becoming ready (XmppProvider handles auth bootstrap)
let hasBootstrapped = false;
watch(
  () => connectionStore.appState,
  (state) => {
    if (state === "ready") {
      void appUpdate.start();
      return;
    }

    appUpdate.stop();
  },
  { immediate: true },
);

watch(
  () => connectionStore.appState,
  (state) => {
    if (state === "ready" && !hasBootstrapped) {
      hasBootstrapped = true;
      void onConnectionReady();
    }
  },
  { immediate: true },
);

onMounted(() => {
  window.addEventListener("popstate", onPopState);
  // keystrok uses "+" as its shortcut separator, so the literal plus key is
  // handled directly while the rest of reaction mode remains scoped there.
  window.addEventListener("keydown", handleLiteralPlusKeyDown, true);
  bindChatKeystrokShortcuts();
});

onUnmounted(() => {
  window.removeEventListener("popstate", onPopState);
  window.removeEventListener("keydown", handleLiteralPlusKeyDown, true);
  chatKeystrok?.destroy();
  chatKeystrok = null;
  appUpdate.stop();
  messaging.disconnect();
  dmMessaging.disconnect();
});
</script>

<template>
  <!-- Loading -->
  <LandingState
    v-if="connectionStore.appState === 'loading'"
    title="Checking session."
  />

  <!-- Signed out -->
  <LoginScreen
    v-else-if="connectionStore.appState === 'signed-out'"
    :default-server-url="connectionStore.activeServerUrl"
    :active-server-url="connectionStore.activeServerUrl"
    :providers="connectionStore.providers"
    :error-message="connectionStore.appError"
    @login="(url, pid) => connectionStore.login(url, pid)"
    @fetch-providers="connectionStore.fetchProviders"
  />

  <!-- Error -->
  <LandingState
    v-else-if="connectionStore.appState === 'error'"
    title="Server unavailable."
    :copy="connectionStore.appError"
    action-label="Try again"
    @action="connectionStore.bootstrap"
  />

  <!-- Ready -->
  <div v-else class="chat-app-shell">
    <!-- Mobile header (settings page only — chat pages render the consolidated header inside ContentArea) -->
    <SettingsMobileHeader
      v-if="ui.activePage.value === 'settings'"
      @open-nav="ui.showMobileNav.value = true"
    />

    <!-- Mobile nav drawer -->
    <AppDrawer v-model:open="ui.showMobileNav.value" side="left" label="Navigation drawer">
      <template #title>
        <span class="type-pane-title">Navigation</span>
      </template>
      <div class="chat-mobile-nav-body">
        <div class="border-b border-border">
          <WaddlesSidebar
            :waddles="[]"
            :active-space-id="null"
            :active-sidebar-mode="ui.sidebarMode.value"
            :has-unread-dms="dmConversations.hasUnread.value"
            :session="null"
            horizontal
            @toggle-channels="ui.sidebarMode.value = 'channels'"
            @toggle-dms="ui.sidebarMode.value = 'dms'"
          />
        </div>
        <TopicsPanel
          v-if="ui.sidebarMode.value === 'channels'"
          :waddle="waddles.currentSpace.value"
          :spaces="waddles.sortedSpaces.value"
          :channels="waddles.sortedChannels.value"
          :active-channel-id="waddles.activeChannelId.value"
          :can-manage-channels="waddles.canManageChannels.value"
          :can-manage-community="waddles.canManageCommunity.value"
          :is-loading="waddles.isLoadingStructure.value"
          :member-count="waddles.members.value.length"
          :active-channel-jids="messaging.activeChannels.value"
          :collapsed-group-ids="ui.collapsedSpaceGroupIds.value"
          :channel-unread-map="computedChannelUnreadMap"
          :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
          class="!w-full !border-r-0 !flex-1"
          @select-channel="selectChannel"
          @select-thread="onSelectThread"
          @create-channel="openCreateChannelDialog()"
          @create-channel-in-space="openCreateChannelDialog"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
          @update-collapsed-group-ids="ui.collapsedSpaceGroupIds.value = $event"
        />
        <DmPanel
          v-else
          :conversations="dmConversations.conversations.value"
          :active-peer-jid="dmConversations.activePeerJid.value"
          class="!w-full !border-r-0 !flex-1"
          @select-dm="selectDm"
          @new-dm="ui.showNewDm.value = true"
        />
        <ProfilePanel
          v-if="connectionStore.session"
          :session="connectionStore.session"
          :notification-permission="notifications.permissionState.value"
          :notifications-enabled="notifications.notificationsEnabled.value"
          :total-unread-count="channelUnread.totalUnreadCount.value"
          :total-mention-count="channelUnread.totalMentionCount.value"
          :web-commit-sha="version.webCommitSha.value"
          :server-version="version.serverVersion.value"
          @open-settings="openUserSettings"
          @logout="handleLogout"
          @request-notifications="handleRequestNotifications"
          @toggle-notifications="handleToggleNotifications"
        />
      </div>
    </AppDrawer>

    <!-- Mobile details drawer -->
    <AppDrawer v-model:open="ui.showMobileDetails.value" side="right" label="Details drawer">
      <template #title>
        <span class="type-pane-title">Details</span>
      </template>
      <div class="flex flex-col gap-4 p-4">
        <div v-if="waddles.currentSpace.value" class="flex flex-col gap-1.5">
          <h3 class="type-pane-title">{{ waddles.currentSpace.value.name }}</h3>
          <p v-if="waddles.currentSpace.value.description" class="type-field text-muted-foreground">
            {{ waddles.currentSpace.value.description }}
          </p>
        </div>

        <div class="flex flex-col gap-1.5">
          <button
            v-if="waddles.currentChannel.value && waddles.canManageChannels.value"
            class="type-control h-9 w-full rounded-lg border border-border px-3 hover:bg-muted transition-colors"
            type="button"
            @click="openChannelEdit(); ui.showMobileDetails.value = false"
          >
            Edit channel
          </button>
          <button
            class="type-control h-9 w-full rounded-lg border border-border px-3 hover:bg-muted transition-colors"
            type="button"
            @click="ui.showMobileDetails.value = false; ui.showMembers.value = true"
          >
            Members ({{ waddles.members.value.length }})
          </button>
        </div>
      </div>
    </AppDrawer>

    <!-- Desktop layout -->
    <div class="chat-desktop-shell">
      <!-- Icon rail: waddle switcher -->
      <div class="chat-desktop-rail-slot">
        <WaddlesSidebar
          :waddles="[]"
          :active-space-id="null"
          :active-sidebar-mode="ui.sidebarMode.value"
          :has-unread-dms="dmConversations.hasUnread.value"
          :session="connectionStore.session"
          :notification-permission="notifications.permissionState.value"
          :notifications-enabled="notifications.notificationsEnabled.value"
          :total-unread-count="channelUnread.totalUnreadCount.value"
          :total-mention-count="channelUnread.totalMentionCount.value"
          :web-commit-sha="version.webCommitSha.value"
          :server-version="version.serverVersion.value"
          @open-settings="openUserSettings"
          @toggle-channels="ui.sidebarMode.value = 'channels'"
          @toggle-dms="ui.sidebarMode.value = 'dms'"
          @logout="handleLogout"
          @request-notifications="handleRequestNotifications"
          @toggle-notifications="handleToggleNotifications"
        />
      </div>

      <!-- Channel sidebar -->
      <div class="chat-sidebar-slot">
        <TopicsPanel
          v-if="ui.sidebarMode.value === 'channels'"
          :waddle="waddles.currentSpace.value"
          :spaces="waddles.sortedSpaces.value"
          :channels="waddles.sortedChannels.value"
          :active-channel-id="waddles.activeChannelId.value"
          :can-manage-channels="waddles.canManageChannels.value"
          :can-manage-community="waddles.canManageCommunity.value"
          :is-loading="waddles.isLoadingStructure.value"
          :member-count="waddles.members.value.length"
          :active-channel-jids="messaging.activeChannels.value"
          :collapsed-group-ids="ui.collapsedSpaceGroupIds.value"
          :channel-unread-map="computedChannelUnreadMap"
          :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
          @select-channel="selectChannel"
          @select-thread="onSelectThread"
          @create-channel="openCreateChannelDialog()"
          @create-channel-in-space="openCreateChannelDialog"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
          @update-collapsed-group-ids="ui.collapsedSpaceGroupIds.value = $event"
        />
        <DmPanel
          v-else
          :conversations="dmConversations.conversations.value"
          :active-peer-jid="dmConversations.activePeerJid.value"
          @select-dm="selectDm"
          @new-dm="ui.showNewDm.value = true"
        />
      </div>

      <!-- Main content -->
      <HomeDashboard
        v-if="ui.activePage.value === 'dashboard'"
        :spaces="waddles.sortedSpaces.value"
        :channels="waddles.sortedChannels.value"
        :contacts="rosterContacts.contacts.value"
        :is-loading="waddles.isLoadingStructure.value || rosterContacts.isLoadingContacts.value"
        @select-channel="selectChannel"
        @select-contact="handleOpenDm"
        @open-nav="ui.showMobileNav.value = true"
      />
      <UserSettingsPage
        v-else-if="ui.activePage.value === 'settings' && connectionStore.session"
        :session="connectionStore.session"
        :xmpp-client="xmppClient"
        :web-commit-sha="version.webCommitSha.value"
        :server-version="version.serverVersion.value"
        @close="closeUserSettings"
      />
      <template v-else>
        <!--
          Accordion thread layout
          ─────────────────────────────────────────────────────────────────────
          Stack depth 0  → ContentArea takes full remaining width
          Stack depth 1  → ContentArea (desktop left pane) + active ThreadPanel
          Stack depth 2+ → parent ThreadPanel (desktop, read-only) + active ThreadPanel

          On mobile only the active thread panel is visible when a thread is open.
          Escape key (handled above) pops back one level in the stack.
        -->

        <!-- ContentArea wrapper:
             - depth 0: visible and flex-1 (full remaining width)
             - depth 1: hidden on mobile, flex-1 on desktop (left pane)
             - depth 2+: hidden entirely (parent thread pane takes its place) -->
        <div class="chat-workspace">
          <div
            :class="[
              'chat-content-pane',
              activeThreadStack.length === 0
                ? ''
                : activeThreadStack.length === 1
                  ? 'chat-content-pane--desktop-split'
                  : 'chat-content-pane--hidden',
            ]"
          >
            <ContentArea
              :ref="setContentAreaRef"
              v-model:draft="activeDraft"
              v-model:forum-title="activeForumTitle"
              :waddle="waddles.currentSpace.value"
              :channel="ui.sidebarMode.value === 'dms' ? null : waddles.currentChannel.value"
              :dm-peer="activeDmPeer"
              :sidebar-mode="ui.sidebarMode.value"
              :messages="activeMessages"
              :first-unseen-id="activeFirstUnseenId"
              :xmpp-status="messaging.xmppStatus.value"
               :action-error="activeActionError"
              :update-available="appUpdate.updateAvailable.value"
              :is-applying-update="appUpdate.isApplyingUpdate.value"
              :is-loading-messages="activeIsLoadingMessages"
              :is-loading-older-messages="activeIsLoadingOlderMessages"
              :has-older-messages="activeHasOlderMessages"
              :is-sending="activeIsSending"
              :can-manage-channels="waddles.canManageChannels.value"
              :member-count="waddles.members.value.length"
              :typing-users="activeTypingUsers"
              :current-user="connectionStore.session?.username"
              :self-domain="selfDomain"
              :avatar-url-by-author="avatarUrlByAuthor"
               :author-jid-by-nick="authorJidByNick"
              :tenor-api-key="tenorApiKey"
               :mention-candidates="mentionCandidates"
              :room-hats="authorHatsByNick"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :search-results="activeSearchResults"
              :is-searching="activeIsSearching"
              :upload-progress="activeUploadProgress"
              :thread-index="threads.index.value"
              :xmpp-client="xmppClient"
              :reaction-mode="reactionModeTarget === 'main' ? reactionModeState : null"
              :ai-assistant-enabled="aiAssistantEnabled"
              @send="sendActiveMessage"
              @typing="notifyActiveComposing"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              @search="searchActiveMessages"
              @clear-search="clearActiveSearch"
              @load-older="loadOlderActiveMessages"
              @edit-channel="openChannelEdit"
              @open-nav="ui.showMobileNav.value = true"
              @open-details="ui.showMobileDetails.value = true"
              @open-dm="handleOpenDm"
              @open-thread="openThread"
              :invoke-extension-action="invokeActiveExtensionAction"
              @refresh-update="refreshAppUpdate"
            />
          </div>

          <!-- Collapsed accordion bars: one per hidden ancestor level (desktop only, depth >= 2).
               Each bar is a thin vertical strip with a rotated label; clicking
               navigates to that level. The channel bar returns to the main feed,
               thread bars collapse levels older than the parent context pane. -->
          <template v-if="ui.sidebarMode.value === 'channels' && activeThreadStack.length >= 2">
            <!-- Channel / main-feed bar -->
            <button
              type="button"
              class="chat-accordion-bar bg-muted/30 hover:bg-muted/60"
              title="Back to channel"
              @click="closeThreadPanel"
            >
              <span class="accordion-bar-label text-muted-foreground/80">
                {{ waddles.currentChannel.value?.name ?? 'Channel' }}
              </span>
            </button>
            <!-- Ancestor thread bars: levels older than the parent (skip last 2 = parent + active) -->
            <button
              v-for="(threadId, i) in activeThreadStack.slice(0, -2)"
              :key="threadId"
              type="button"
              class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
              :title="getThreadLabel(threadId)"
              @click="popThreadTo(i)"
            >
              <span class="accordion-bar-label text-muted-foreground/60">
                {{ getThreadLabel(threadId) }}
              </span>
            </button>
          </template>

          <!-- Parent thread pane: desktop-only context when depth >= 2.
               Shows the second-to-last thread in read-only mode (no composer). -->
          <div
            v-if="ui.sidebarMode.value === 'channels' && activeThreadStack.length >= 2"
            class="chat-parent-thread-pane"
          >
            <ThreadPanel
              :thread-stack="activeThreadStack.slice(0, -1)"
              :thread-index="threads.index.value"
              :resolve-entry="threads.resolveEntry"
              :current-user="connectionStore.session?.username"
              :avatar-url-by-author="avatarUrlByAuthor"
               :author-jid-by-nick="authorJidByNick"
              :room-hats="authorHatsByNick"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :tenor-api-key="tenorApiKey"
               :mention-candidates="mentionCandidates"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :is-sending="false"
              :is-loading-older-replies="messaging.loadingOlderThreadIds.value.has(activeThreadStack[activeThreadStack.length - 2] ?? '')"
              :has-older-replies="messaging.threadHasOlder.value[activeThreadStack[activeThreadStack.length - 2] ?? ''] ?? false"
              :upload-progress="{ uploading: false, progress: 0, filename: '' }"
              :channel-name="waddles.currentChannel.value?.name ?? ''"
              :ai-assistant-enabled="aiAssistantEnabled"
              :hide-composer="true"
              :reaction-mode="null"
              @close="closeThreadPanel"
              @pop-to="popThreadTo"
              @push-thread="pushThread"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              :invoke-extension-action="invokeActiveExtensionAction"
              @displayed="markActiveDisplayed"
              @load-older="loadOlderThreadMessages"
            />
          </div>

          <!-- Active thread pane: shown when any thread is open.
               Full-width on mobile; shares space with parent context on desktop. -->
          <div
            v-if="ui.sidebarMode.value === 'channels' && activeThreadStack.length >= 1"
            class="chat-active-thread-pane"
          >
            <ThreadPanel
              :thread-stack="activeThreadStack"
              :thread-index="threads.index.value"
              :resolve-entry="threads.resolveEntry"
              :current-user="connectionStore.session?.username"
              :avatar-url-by-author="avatarUrlByAuthor"
               :author-jid-by-nick="authorJidByNick"
              :room-hats="authorHatsByNick"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :tenor-api-key="tenorApiKey"
               :mention-candidates="mentionCandidates"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :is-sending="messaging.isSending.value"
              :is-loading-older-replies="messaging.loadingOlderThreadIds.value.has(activeThreadStack[activeThreadStack.length - 1] ?? '')"
              :has-older-replies="messaging.threadHasOlder.value[activeThreadStack[activeThreadStack.length - 1] ?? ''] ?? false"
              :upload-progress="messaging.uploadProgress.value"
              :channel-name="waddles.currentChannel.value?.name ?? ''"
              :ai-assistant-enabled="aiAssistantEnabled"
              :reaction-mode="reactionModeTarget === 'thread' ? reactionModeState : null"
              @close="closeThreadPanel"
              @pop-to="popThreadTo"
              @push-thread="pushThread"
              @send="sendThreadMessage"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              :invoke-extension-action="invokeActiveExtensionAction"
              @displayed="markActiveDisplayed"
              @select-gif="sendGif"
              @typing="notifyActiveComposing"
              @load-older="loadOlderThreadMessages"
            />
          </div>
        </div>
      </template>
    </div>

    <!-- Dialogs -->
    <NewDmDialog
      v-model:open="ui.showNewDm.value"
      @submit="handleNewDm"
    />
    <CreateChannelDialog
      v-model:open="ui.showCreateChannel.value"
      :form="waddles.createChannelForm.value"
      :is-submitting="waddles.isSubmitting.value"
      :spaces="waddles.sortedSpaces.value.map((space) => ({ node: space.id, name: space.name }))"
      :default-space-node="ui.createChannelContextSpaceId.value"
      @update:form="waddles.createChannelForm.value = $event"
      @submit="handleCreateChannel"
    />

    <WaddleSettingsDialog
      v-model:open="ui.showWaddleSettings.value"
      :waddle="waddles.currentSpace.value"
      :form="waddles.editWaddleForm.value"
      :is-submitting="waddles.isSubmitting.value"
      @update:form="waddles.editWaddleForm.value = $event"
      @save="handleUpdateWaddle"
      @delete="handleDeleteWaddle"
    />

    <EditChannelDialog
      v-model:open="ui.showEditChannel.value"
      :channel="waddles.currentChannel.value"
      :form="waddles.editChannelForm.value"
      :is-submitting="waddles.isSubmitting.value"
      @update:form="waddles.editChannelForm.value = $event"
      @save="handleUpdateChannel"
      @delete="handleDeleteChannel"
    />

    <MemberManagement
      v-model:open="ui.showMembers.value"
      :members="membersWithAvatars"
      :member-query="members.memberQuery.value"
      :new-member-role="members.newMemberRole.value"
      :search-results="members.memberSearchResults.value"
      :is-searching="members.isSearchingUsers.value"
      :can-manage-members="waddles.canManageMembers.value"
      :room-presence="messaging.roomPresence.value"
      :room-last-seen="messaging.roomLastSeen.value"
      @update:member-query="members.memberQuery.value = $event"
      @update:new-member-role="members.newMemberRole.value = $event"
      @add-member="members.addMember"
      @update-role="members.updateMemberRole"
      @remove-member="handleRemoveMember"
    />

    <!-- Confirmation dialogs -->
    <ConfirmDialog
      v-model:open="ui.confirmDeleteWaddle.value"
      title="Delete Waddle?"
      :message="`Are you sure you want to delete ${waddles.currentSpace.value?.name ?? 'this waddle'}? All channels and messages will be permanently removed.`"
      confirm-label="Delete Waddle"
      destructive
      :loading="waddles.isSubmitting.value"
      @confirm="confirmDeleteWaddle"
    />

    <ConfirmDialog
      v-model:open="ui.confirmDeleteChannel.value"
      title="Delete channel?"
      :message="`Are you sure you want to delete #${waddles.currentChannel.value?.name ?? 'this channel'}? All messages in this channel will be permanently removed.`"
      confirm-label="Delete channel"
      destructive
      :loading="waddles.isSubmitting.value"
      @confirm="confirmDeleteChannel"
    />

    <ConfirmDialog
      :open="ui.confirmRemoveMember.value !== null"
      title="Remove member?"
      :message="`Are you sure you want to remove ${ui.confirmRemoveMember.value ?? 'this member'} from the waddle?`"
      confirm-label="Remove member"
      destructive
      @update:open="(v: boolean) => { if (!v) ui.confirmRemoveMember.value = null }"
      @confirm="confirmRemoveMember"
    />
  </div>
</template>
