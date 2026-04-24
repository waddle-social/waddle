<script setup lang="ts">
import { type ComponentPublicInstance, computed, onMounted, onUnmounted, ref, watch, watchEffect } from "vue";
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
import { useVersion } from "@/composables/useVersion";
import { buildDmPath, buildPath, buildSettingsPath, parseRoute, pushDmRoute, pushRoute, pushSettingsRoute, resolveChannel } from "@/composables/useRouting";
import { barePeerJid, jidDomain, parseManagedRoomBareJid, roomBareJidFor } from "@/lib/xmpp-client";
import { connectionStore } from "@/lib/connection-store";
import LandingState from "@/components/chat/LandingState.vue";
import LoginScreen from "@/components/chat/LoginScreen.vue";
import WaddlesSidebar from "@/components/chat/WaddlesSidebar.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import DmPanel from "@/components/chat/DmPanel.vue";
import ContentArea from "@/components/chat/ContentArea.vue";
import ThreadPanel from "@/components/chat/ThreadPanel.vue";
import MobileHeader from "@/components/chat/MobileHeader.vue";
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
import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";
import { mentionMatchesUsername } from "@/lib/mentions";

const props = defineProps<{
  tenorApiKey?: string;
}>();

const tenorApiKey = props.tenorApiKey ?? "";

const ui = useUiState();

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

const members = useMembers(
  xmppClient,
  waddles.activeSpaceId,
  waddles.activeChannelId,
  waddles.members,
  waddles.canManageMembers,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
  waddles.loadStructure,
);

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
);

const dmConversations = useDmConversations(
  session,
  xmppClient,
);

const channelUnread = useChannelUnread(xmppClient);

const dmMessaging = useDmMessaging(
  session,
  xmppClient,
  dmConversations.activePeerJid,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
);

const contentAreaRef = ref<ComponentPublicInstance & { messagesContainer: HTMLDivElement | null } | null>(null);
const setContentAreaRef = (
  instance: (ComponentPublicInstance & { messagesContainer: HTMLDivElement | null }) | null,
) => {
  contentAreaRef.value = instance;
};

watchEffect(() => {
  const timeline = contentAreaRef.value?.messagesContainer ?? null;
  if (ui.sidebarMode.value === "dms") {
    dmMessaging.timelineEl.value = timeline;
    messaging.timelineEl.value = null;
  } else {
    messaging.timelineEl.value = timeline;
    dmMessaging.timelineEl.value = null;
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

function getThreadLabel(threadId: string): string {
  const entry = threads.resolveEntry(threadId);
  const body = entry?.root?.body?.trim() ?? "";
  return body.length > 0 ? body.slice(0, 40) : threadId.slice(0, 8);
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
const memberJidByNick = ref<Record<string, string>>({});
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

const notifications = useNotifications();
const appUpdate = useAppUpdate();
const version = useVersion(xmppClient);
const avatarUrlByAuthor = computed<Record<string, string | null>>(() => {
  const avatars: Record<string, string | null> = {};

  if (session.value) {
    avatars[session.value.username] = session.value.avatar_url;
  }

  for (const member of waddles.members.value) {
    if (!(member.username in avatars) || member.avatar_url) {
      avatars[member.username] = member.avatar_url;
    }
  }

  return avatars;
});

function resolveChannelNameFromJid(roomJid: string): string | null {
  const managedRoom = parseManagedRoomBareJid(roomJid);
  if (!managedRoom || !waddles.activeSpaceId.value) return null;
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
        void selectSpace(managedRoom.channelId);
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
  client.setPresenceUpdateHandler(dmConversations.updatePresence);
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
  const known = messaging.messages.value.some((m) => m.id === threadId);
  if (!known) {
    void messaging.backfillThread(threadId);
  }
}

async function onSelectThread(channelId: string, threadId: string) {
  // Navigate to the channel if not already there
  if (waddles.activeChannelId.value !== channelId) {
    await selectChannel(channelId);
  }
  // Mark thread as read
  if (waddles.activeSpaceId.value && connectionStore.session) {
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
  const known = messaging.messages.value.some((m) => m.id === threadId);
  if (!known) {
    void messaging.backfillThread(threadId);
  }
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

function searchActiveMessages(query: string) {
  void activeTarget.value.searchMessages(query);
}

function clearActiveSearch() {
  activeTarget.value.clearSearch();
}

// --- Deep linking ---

function updateUrl() {
  if (isApplyingRoute.value) return;
  if (ui.activePage.value === "settings") {
    pushSettingsRoute("app");
    return;
  }
  if (ui.sidebarMode.value === "dms" && activeDmPeer.value) {
    pushDmRoute(activeDmPeer.value.peerUsername);
  } else {
    pushRoute(waddles.currentChannel.value, activeThreadStack.value);
  }
}

watch(
  [waddles.activeSpaceId, waddles.activeChannelId, ui.sidebarMode, () => dmConversations.activePeerJid.value],
  () => {
    // Channel / DM / mode changes close any open thread panel — the ids inside
    // the stack belong to the channel we just left.
    activeThreadStack.value = [];
    updateUrl();
  },
);

watch(activeThreadStack, updateUrl, { deep: true });
watch(() => ui.activePage.value, updateUrl);

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
  ui.activePage.value = "chat";
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
  if (route.dmUsername) {
    const username = route.dmUsername.replace(/^@/, "").trim();
    if (username) {
      const domain = session.value ? jidDomain(session.value.jid) : "";
      if (!domain) return;
      await handleOpenDm(`${username}@${domain}`);
    }
    return;
  }

  if (waddles.activeSpaceId.value && waddles.channels.value.length === 0) {
    await waddles.loadStructure();
    if (requestId !== routeRequestId) return;
  }

  if (route.channelSlug && waddles.activeSpaceId.value) {
    const ch = resolveChannel(route.channelSlug, waddles.channels.value);
    const channelId = ch?.id ?? waddles.activeChannelId.value;
    if (channelId) {
      waddles.activeChannelId.value = channelId;
      messaging.clearMessages();
      await messaging.loadMessages(waddles.activeSpaceId.value, channelId);
    }
  } else if (waddles.activeSpaceId.value && waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.activeSpaceId.value, waddles.activeChannelId.value);
  }

  // Restore the thread panel from the URL. If the outermost thread root isn't
  // in the freshly loaded history, MAM-backfill it so the panel has something
  // to render.
  activeThreadStack.value = route.threadStack;
  const outerThreadId = route.threadStack[0];
  if (outerThreadId) {
    const known = messaging.messages.value.some((m) => m.id === outerThreadId);
    if (!known) {
      void messaging.backfillThread(outerThreadId);
    }
  }
}

// --- Bootstrap (watches connection store) ---

async function onConnectionReady() {
  const route = parseRoute(window.location.pathname, window.location.search);
  const requestId = ++routeRequestId;
  isApplyingRoute.value = true;

  try {
    await waddles.loadSpace({ loadStructure: !route.channelSlug });
    if (requestId === routeRequestId) {
      await applyRouteTarget(route, requestId);
    }
  } finally {
    if (requestId === routeRequestId) {
      isApplyingRoute.value = false;
      updateUrl();
    }
  }

  void dmConversations.hydrateFromInbox();
  void channelUnread.hydrateFromInbox();

  // Register service worker and sync push subscription (best-effort, non-blocking)
  void (async () => {
    await notifications.registerServiceWorker();
    await setupPushSubscription();
  })();
}

// --- Actions ---

async function handleLogout() {
  ui.activePage.value = "chat";
  messaging.disconnect();
  dmMessaging.disconnect();
  waddles.clearData();
  messaging.clearMessages();
  dmMessaging.clearMessages();
  pushRoute(null);
  await connectionStore.logout();
}

async function selectSpace(preferredChannelId?: string | null) {
  ui.activePage.value = "chat";
  ui.sidebarMode.value = "channels";
  dmConversations.closeDm();
  const channelId = await waddles.loadStructure(preferredChannelId);
  if (channelId && waddles.activeSpaceId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.activeSpaceId.value, channelId);
  }
  ui.showMobileNav.value = false;
}

async function selectChannel(channelId: string) {
  ui.activePage.value = "chat";
  ui.sidebarMode.value = "channels";
  dmConversations.closeDm();
  memberJidByNick.value = {};
  waddles.activeChannelId.value = channelId;
  messaging.clearMessages();
  // XEP-0502: Clear activity indicator for this channel
  if (waddles.activeSpaceId.value && connectionStore.session) {
    const roomJid = roomBareJidFor(connectionStore.session, channelId);
    messaging.clearChannelActivity(roomJid);
    channelUnread.markRead(roomJid);
  }
  if (waddles.activeSpaceId.value) {
    await messaging.loadMessages(waddles.activeSpaceId.value, channelId);
  }
  ui.showMobileNav.value = false;
}

async function handleOpenDm(peerJid: string) {
  ui.activePage.value = "chat";
  ui.sidebarMode.value = "dms";
  await dmConversations.openDm(peerJid);
  dmMessaging.clearMessages();
  if (dmConversations.activePeerJid.value) {
    await dmMessaging.loadMessages(dmConversations.activePeerJid.value);
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
  if (created) {
    ui.showCreateChannel.value = false;
    if (waddles.activeSpaceId.value) {
      messaging.clearMessages();
      await messaging.loadMessages(waddles.activeSpaceId.value, created.id);
    }
  }
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
  if (waddles.activeSpaceId.value && waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.activeSpaceId.value, waddles.activeChannelId.value);
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
  if (waddles.activeSpaceId.value && waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.activeSpaceId.value, waddles.activeChannelId.value);
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

function handleKeyDown(e: KeyboardEvent) {
  if (e.key !== "Escape") return;
  if (activeThreadStack.value.length === 0) return;
  // Don't intercept Escape when any dialog/drawer is open so they can close first.
  const anyModalOpen =
    ui.showCreateChannel.value ||
    ui.showEditChannel.value ||
    ui.showWaddleSettings.value ||
    ui.showMembers.value ||
    ui.confirmDeleteWaddle.value ||
    ui.confirmDeleteChannel.value ||
    ui.showNewDm.value ||
    ui.confirmRemoveMember.value !== null ||
    ui.showMobileNav.value ||
    ui.showMobileDetails.value;
  if (anyModalOpen) return;
  activeThreadStack.value = activeThreadStack.value.slice(0, -1);
  e.preventDefault();
}

onMounted(() => {
  window.addEventListener("popstate", onPopState);
  window.addEventListener("keydown", handleKeyDown);
});

onUnmounted(() => {
  window.removeEventListener("popstate", onPopState);
  window.removeEventListener("keydown", handleKeyDown);
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
    <!-- Mobile header -->
    <MobileHeader
      :page="ui.activePage.value"
      :waddle="waddles.currentSpace.value"
      :channel="waddles.currentChannel.value"
      :dm-peer="activeDmPeer"
      :sidebar-mode="ui.sidebarMode.value"
      :session="connectionStore.session"
      @open-nav="ui.showMobileNav.value = true"
      @open-details="ui.showMobileDetails.value = true"
    />

    <!-- Mobile nav drawer -->
    <AppDrawer v-model:open="ui.showMobileNav.value" side="left" label="Navigation drawer">
      <template #title>
        <span class="type-pane-title">Navigation</span>
      </template>
      <div class="chat-mobile-nav-body">
        <div class="border-b border-border">
          <WaddlesSidebar
            :waddles="waddles.sortedSpaces.value"
            :active-space-id="waddles.activeSpaceId.value"
            :active-sidebar-mode="ui.sidebarMode.value"
            :has-unread-dms="dmConversations.hasUnread.value"
            :session="null"
            horizontal
            @select-space="selectSpace($event)"
            @toggle-dms="ui.sidebarMode.value = 'dms'"
          />
        </div>
        <TopicsPanel
          v-if="ui.sidebarMode.value === 'channels'"
          :waddle="waddles.currentSpace.value"
          :channels="waddles.sortedChannels.value"
          :active-channel-id="waddles.activeChannelId.value"
          :can-manage-channels="waddles.canManageChannels.value"
          :can-manage-community="waddles.canManageCommunity.value"
          :is-loading="waddles.isLoadingStructure.value"
          :member-count="waddles.members.value.length"
          :active-channel-jids="messaging.activeChannels.value"
          :channel-unread-map="computedChannelUnreadMap"
          :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
          class="!w-full !border-r-0 !flex-1"
          @select-channel="selectChannel"
          @select-thread="onSelectThread"
          @create-channel="ui.showCreateChannel.value = true"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
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
          :waddles="waddles.sortedSpaces.value"
          :active-space-id="waddles.activeSpaceId.value"
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
          @select-space="selectSpace($event)"
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
          :channels="waddles.sortedChannels.value"
          :active-channel-id="waddles.activeChannelId.value"
          :can-manage-channels="waddles.canManageChannels.value"
          :can-manage-community="waddles.canManageCommunity.value"
          :is-loading="waddles.isLoadingStructure.value"
          :member-count="waddles.members.value.length"
          :active-channel-jids="messaging.activeChannels.value"
          :channel-unread-map="computedChannelUnreadMap"
          :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
          @select-channel="selectChannel"
          @select-thread="onSelectThread"
          @create-channel="ui.showCreateChannel.value = true"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
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
      <UserSettingsPage
        v-if="ui.activePage.value === 'settings' && connectionStore.session"
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
              :action-error="ui.actionError.value"
              :update-available="appUpdate.updateAvailable.value"
              :is-applying-update="appUpdate.isApplyingUpdate.value"
              :is-loading-messages="activeIsLoadingMessages"
              :is-sending="activeIsSending"
              :can-manage-channels="waddles.canManageChannels.value"
              :typing-users="activeTypingUsers"
              :current-user="connectionStore.session?.username"
              :self-domain="selfDomain"
              :avatar-url-by-author="avatarUrlByAuthor"
              :author-jid-by-nick="memberJidByNick"
              :tenor-api-key="tenorApiKey"
              :member-names="waddles.members.value.map((m) => m.username)"
              :room-hats="messaging.roomHats.value"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :search-results="activeSearchResults"
              :is-searching="activeIsSearching"
              :upload-progress="activeUploadProgress"
              :thread-index="threads.index.value"
              :xmpp-client="xmppClient"
              @send="sendActiveMessage"
              @typing="notifyActiveComposing"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              @displayed="markActiveDisplayed"
              @search="searchActiveMessages"
              @clear-search="clearActiveSearch"
              @edit-channel="openChannelEdit"
              @open-dm="handleOpenDm"
              @open-thread="openThread"
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
              :author-jid-by-nick="memberJidByNick"
              :room-hats="messaging.roomHats.value"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :tenor-api-key="tenorApiKey"
              :member-names="waddles.members.value.map((m) => m.username)"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :is-sending="false"
              :upload-progress="{ uploading: false, progress: 0, filename: '' }"
              :channel-name="waddles.currentChannel.value?.name ?? ''"
              :hide-composer="true"
              @close="closeThreadPanel"
              @pop-to="popThreadTo"
              @push-thread="pushThread"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              @displayed="markActiveDisplayed"
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
              :author-jid-by-nick="memberJidByNick"
              :room-hats="messaging.roomHats.value"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :tenor-api-key="tenorApiKey"
              :member-names="waddles.members.value.map((m) => m.username)"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :is-sending="messaging.isSending.value"
              :upload-progress="messaging.uploadProgress.value"
              :channel-name="waddles.currentChannel.value?.name ?? ''"
              @close="closeThreadPanel"
              @pop-to="popThreadTo"
              @push-thread="pushThread"
              @send="sendThreadMessage"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              @displayed="markActiveDisplayed"
              @select-gif="sendGif"
              @typing="notifyActiveComposing"
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
      :members="waddles.sortedMembers.value"
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
