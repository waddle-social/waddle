import { computed, ref } from "vue";
import { useWaddleDirectory } from "@/waddles/directory";
import { useWaddleMembers } from "@/waddles/members";
import { useDirectMessageConversations } from "@/dms/conversations";
import { useDirectMessages } from "@/dms/messages";
import { useChannelMessages } from "@/channels/messages";
import { useMessageThreads } from "@/channels/threads";
import { useChatShellState } from "@/shell/state";
import { useServiceWorkerUpdate } from "@/shell/service-worker-update";
import { usePushNotifications } from "@/shell/notifications";
import { createBrowserMessageTonePlayer } from "@/shell/audio-alerts";
import { useChatWindowVisibility } from "@/shell/window-visibility";
import { useChannelInbox } from "@/channels/inbox";
import { useSocialFeed } from "@/services/social-feed";
import { useStories } from "@/services/stories";
import { useCommunityEvents } from "@/services/community-events";
import { useChatReadActivity } from "@/shell/read-activity";
import { useDeploymentVersionInfo } from "@/shell/version";
import { useXmppRosterContacts } from "@/contacts/roster";
import { matchLocation } from "@/router";
import { jidDomain, jidDomainOrEmpty } from "@/lib/xmpp-client";
import { createNotifySettingsStore } from "@/lib/notify-settings";
import { roomJidForChannelId as resolveRoomJidForChannelId } from "@/lib/channel-room";
import { normalizeMucServiceDomain } from "@/lib/calls/muc-call-indicators";
import { connectionStore } from "@/lib/connection-store";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import { useActiveConversation } from "@/shell/controllers/use-active-conversation";
import { useMemberDirectory } from "@/shell/controllers/use-member-directory";
import { usePresenceSync } from "@/shell/controllers/use-presence-sync";
import { useExtensionRoutes, type ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";
import { useRoomSync } from "@/shell/controllers/use-room-sync";
import { useDmSync } from "@/shell/controllers/use-dm-sync";
import { useThreadPanels, type ActiveRightPanel } from "@/shell/controllers/use-thread-panels";
import { usePinnedMessages } from "@/shell/controllers/use-pinned-messages";
import { useSendOrchestration } from "@/shell/controllers/use-send-orchestration";
import { useReactionMode } from "@/shell/controllers/use-reaction-mode";
import { anyModalOpen, useChatKeyboard, type KeystrokHandle } from "@/shell/controllers/use-chat-keyboard";
import { useNotificationOrchestration } from "@/shell/controllers/use-notification-orchestration";
import { usePageNavigation } from "@/shell/controllers/use-page-navigation";
import { useWorkspaceDialogs } from "@/shell/controllers/use-workspace-dialogs";
import { applyMatchToShellState, useRouteSync } from "@/shell/controllers/use-route-sync";
import { useConnectionLifecycle } from "@/shell/controllers/use-connection-lifecycle";

/**
 * Composition root for the chat shell. Every feature lives in a focused
 * composable under `./controllers/`; this function instantiates them,
 * wires the shared reactive state between them explicitly (refs and
 * composable instances passed as parameters — no implicit module-level
 * coupling), and assembles the controller object AppShell publishes via
 * the `appController` store.
 */
export function useChatAppController() {
  const ui = useChatShellState();
  const { mode: scrollDirectionMode } = useScrollDirectionPreference();
  const { isWindowFocused } = useChatWindowVisibility();

  // Seed page-level UI state from the current URL so the first render
  // lands on the right page. Without this, every cold load flashes the
  // default home dashboard / channels-sidebar for one tick before
  // `onConnectionReady` runs `applyRouteTarget` and snaps to the real
  // page.
  if (typeof window !== "undefined") {
    applyMatchToShellState(ui, matchLocation(window.location.pathname, window.location.search));
  }

  const xmppClient = computed(() => connectionStore.client);
  const session = computed(() => connectionStore.session);
  // Per-controller XEP-0492 store. Constructed here (not a
  // module-level singleton) so unrelated consumers can't share
  // state implicitly — PR-review compliance. Exposed on the
  // controller return and threaded into child components via
  // explicit props.
  const notifySettings = createNotifySettingsStore();

  const waddles = useWaddleDirectory(
    xmppClient,
    session,
    ui.normalizeError,
    ui.actionError,
    ui.clearActionError,
  );

  const memberJidByNick = ref<Record<string, string>>({});
  const mentionJidsByNickForSend = ref<Record<string, string>>({});

  const messaging = useChannelMessages(
    session,
    xmppClient,
    waddles.activeSpaceId,
    waddles.activeChannelId,
    waddles.currentChannel,
    ui.normalizeError,
    ui.actionError,
    ui.clearActionError,
    mentionJidsByNickForSend,
    computed(() => ui.activePage.value === "chat" && ui.sidebarMode.value === "channels"),
  );

  const dmConversations = useDirectMessageConversations(
    session,
    xmppClient,
  );

  const channelUnread = useChannelInbox(xmppClient);
  const rosterContacts = useXmppRosterContacts(xmppClient);

  // Community service (`community.<user-domain>`) hosts the
  // XEP-0472 social feed and XEP-0501 stories nodes. Kept distinct
  // from the spaces service so the spaces sidebar enumerates only
  // real community spaces, not pubsub leaf nodes.
  const communityJid = computed(() => {
    const jid = session.value?.jid;
    if (!jid) return null;
    const domain = jidDomainOrEmpty(jid);
    return domain ? `community.${domain}` : null;
  });
  const socialFeed = useSocialFeed(xmppClient, { communityJid });
  const stories = useStories(xmppClient, { communityJid });
  const communityEvents = useCommunityEvents(xmppClient, { communityJid });

  const dmMessaging = useDirectMessages(
    session,
    xmppClient,
    dmConversations.activePeerJid,
    ui.normalizeError,
    ui.actionError,
    ui.clearActionError,
    dmConversations.activeConversationScope,
  );

  // --- Shared reactive state wired explicitly between the feature
  // composables. Each ref has one conceptual owner (the composable whose
  // watchers keep it consistent) but is created here because several
  // features read or reset it.
  const isApplyingRoute = ref(false);
  const activeThreadStack = ref<string[]>([]);
  const activeThreadTargetMessageId = ref<string | null>(null);
  const activeThreadTargetRequestId = ref(0);
  const activeRightPanel = ref<ActiveRightPanel | null>(null);
  const extensionRoutes = ref<DiscoveredExtensionRoute[]>([]);
  const activeExtensionRouteKey = ref<ExtensionRouteKey | null>(null);
  const keystrok: KeystrokHandle = { current: null };

  // Late-bound handles for the few genuinely circular seams (URL sync is
  // derived from panel/room state, while panel/room actions push the URL).
  // The thunks below are only invoked from watchers and user events, which
  // fire after setup completes, so the handles are always assigned by then.
  let routeSyncHandle: ReturnType<typeof useRouteSync> | null = null;
  let reactionModeHandle: ReturnType<typeof useReactionMode> | null = null;
  let lifecycleHandle: ReturnType<typeof useConnectionLifecycle> | null = null;
  const updateUrl = () => routeSyncHandle?.updateUrl();
  const exitReactionMode = () => reactionModeHandle?.exitReactionMode();
  const clearPendingChannelRoute = () => lifecycleHandle?.clearPendingChannelRoute();

  const conversation = useActiveConversation({
    ui,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    isApplyingRoute,
  });

  const selfDomain = computed(() => (session.value ? jidDomain(session.value.jid) : ""));
  const managedMucDomain = computed(() =>
    normalizeMucServiceDomain(waddles.mucServiceJid.value) || (selfDomain.value ? `muc.${selfDomain.value}` : ""),
  );

  // Thread panel state - stack = breadcrumb trail into nested sub-threads.
  // Empty stack = panel closed. Channels and DMs both use XEP-0201 threads.
  const threads = useMessageThreads(conversation.activeMessages);

  const members = useWaddleMembers(
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

  const memberDirectory = useMemberDirectory({
    xmppClient,
    session,
    waddles,
    messaging,
    dmMessaging,
    memberJidByNick,
    mentionJidsByNickForSend,
    selfDomain,
  });

  const { computedChannelUnreadMap, totalTabUnreadCount } = useChatReadActivity({
    appReady: computed(() => connectionStore.appState === "ready"),
    session,
    xmppClient,
    activePage: ui.activePage,
    sidebarMode: ui.sidebarMode,
    activeChannelId: waddles.activeChannelId,
    channels: waddles.channels,
    channelUnread,
    dmConversations,
    messaging,
    dmMessaging,
    activeTarget: conversation.activeTarget,
    roomJidForChannelId: resolveRoomJidForChannelId,
  });
  const groupDmConversations = computed(() =>
    waddles.groupDms.value.map((group) => {
      const activity = computedChannelUnreadMap.value[group.id];
      return {
        ...group,
        unreadCount: activity?.unread ?? 0,
        mentionCount: activity?.mentions ?? 0,
      };
    }),
  );

  const notifications = usePushNotifications();
  const messageSound = createBrowserMessageTonePlayer();

  const presence = usePresenceSync({
    xmppClient,
    session,
    dmConversations,
    rosterContacts,
  });

  const appUpdate = useServiceWorkerUpdate();
  const version = useDeploymentVersionInfo(xmppClient);

  const roomSync = useRoomSync({
    ui,
    xmppClient,
    session,
    waddles,
    messaging,
    dmConversations,
    computedChannelUnreadMap,
    managedMucDomain,
    memberJidByNick,
    activeExtensionRouteKey,
    updateUrl,
  });

  const extension = useExtensionRoutes({
    ui,
    xmppClient,
    waddles,
    messaging,
    dmConversations,
    extensionRoutes,
    activeExtensionRouteKey,
    activeRightPanel,
    memberJidByNick,
    clearPendingChannelRoomJidSelection: roomSync.clearPendingChannelRoomJidSelection,
    updateUrl,
  });

  const dmSync = useDmSync({
    ui,
    xmppClient,
    session,
    waddles,
    dmConversations,
    dmMessaging,
    rosterContacts,
    selfDomain,
    activeExtensionRouteKey,
    clearPendingChannelRoomJidSelection: roomSync.clearPendingChannelRoomJidSelection,
    selectGroupDm: roomSync.selectGroupDm,
  });

  const panels = useThreadPanels({
    ui,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    channelUnread,
    threads,
    isActiveDirectDmSurface: conversation.isActiveDirectDmSurface,
    activeChannelRoomJid: roomSync.activeChannelRoomJid,
    managedMucDomain,
    activeThreadStack,
    activeThreadTargetMessageId,
    activeThreadTargetRequestId,
    activeRightPanel,
    activeExtensionRouteKey,
    roomJidForChannelId: roomSync.roomJidForChannelId,
    selectChannel: roomSync.selectChannel,
    openDm: dmSync.handleOpenDm,
    exitReactionMode,
    updateUrl,
  });

  const pinned = usePinnedMessages({
    ui,
    xmppClient,
    session,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    isActiveDirectDmSurface: conversation.isActiveDirectDmSurface,
    activeTarget: conversation.activeTarget,
    contentAreaRef: conversation.contentAreaRef,
  });

  const send = useSendOrchestration({
    ui,
    xmppClient,
    waddles,
    messaging,
    dmMessaging,
    isActiveDirectDmSurface: conversation.isActiveDirectDmSurface,
    activeTarget: conversation.activeTarget,
    activeDmPeer: conversation.activeDmPeer,
  });

  const reactionMode = useReactionMode({
    ui,
    scrollDirectionMode,
    waddles,
    threads,
    activeMessages: conversation.activeMessages,
    activeThreadStack,
    activeRightPanel,
    activeDmPeer: conversation.activeDmPeer,
    reactMessage: send.reactActiveMessage,
    anyModalOpen: () => anyModalOpen(ui),
    keystrok,
  });
  reactionModeHandle = reactionMode;

  useChatKeyboard({
    ui,
    keystrok,
    activeRightPanel,
    activeExtensionRouteKey,
    activeThreadStack,
    closeExtensionRoutePanel: panels.closeExtensionRoutePanel,
    closePinnedPanel: panels.closePinnedPanel,
    normalizeActiveRightPanel: panels.normalizeActiveRightPanel,
    reactionMode,
  });

  const notificationOrchestration = useNotificationOrchestration({
    ui,
    xmppClient,
    session,
    waddles,
    dmConversations,
    notifications,
    notifySettings,
    messageSound,
    isWindowFocused,
    isSelfDoNotDisturb: presence.isSelfDoNotDisturb,
    pendingNotificationActivities: messaging.pendingNotificationActivities,
    selectChannel: roomSync.selectChannel,
    openDm: dmSync.handleOpenDm,
  });

  const pageNavigation = usePageNavigation({
    ui,
    waddles,
    dmConversations,
    activeDmPeer: conversation.activeDmPeer,
    activeRightPanel,
    activeThreadStack,
    activeExtensionRouteKey,
    clearPendingChannelRoomJidSelection: roomSync.clearPendingChannelRoomJidSelection,
    updateUrl,
  });

  const workspaceDialogs = useWorkspaceDialogs({
    ui,
    connectionStore,
    waddles,
    messaging,
    members,
    activeExtensionRouteKey,
  });

  const routeSync = useRouteSync({
    ui,
    session,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    isApplyingRoute,
    activeDmPeer: conversation.activeDmPeer,
    activeThreadStack,
    activeThreadTargetMessageId,
    activeRightPanel,
    activeExtensionRouteKey,
    clearPendingChannelRoomJidSelection: roomSync.clearPendingChannelRoomJidSelection,
    openDm: dmSync.handleOpenDm,
    selectGroupDm: roomSync.selectGroupDm,
    clearPendingChannelRoute,
  });
  routeSyncHandle = routeSync;

  const lifecycle = useConnectionLifecycle({
    ui,
    connectionStore,
    xmppClient,
    session,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    channelUnread,
    rosterContacts,
    socialFeed,
    stories,
    communityEvents,
    notifications,
    notifySettings,
    appUpdate,
    isApplyingRoute,
    memberJidByNick,
    extensionRoutes,
    activeExtensionRouteKey,
    activeRightPanel,
    selectedChannelRoomJids: roomSync.selectedChannelRoomJids,
    isActiveDirectDmSurface: conversation.isActiveDirectDmSurface,
    presence,
    notificationOrchestration,
    routeSync,
    refreshExtensionRoutes: extension.refreshExtensionRoutes,
    clearPendingChannelRoomJidSelection: roomSync.clearPendingChannelRoomJidSelection,
    showFirstRunSetupIfNeeded: workspaceDialogs.showFirstRunSetupIfNeeded,
    resetSetupPrompt: workspaceDialogs.resetSetupPrompt,
  });
  lifecycleHandle = lifecycle;

  async function refreshAppUpdate() {
    await appUpdate.applyUpdate();
  }

  return {
    connectionStore,
    ui,
    waddles,
    messaging,
    dmConversations,
    channelUnread,
    rosterContacts,
    socialFeed,
    stories,
    communityEvents,
    communityJid,
    dmMessaging,
    xmppClient,
    notifySettings,
    activeMessages: conversation.activeMessages,
    activeFirstUnseenId: conversation.activeFirstUnseenId,
    extensionRoutes,
    channelExtensionRoutes: extension.channelExtensionRoutes,
    activeExtensionRouteKey,
    activeExtensionRoute: extension.activeExtensionRoute,
    activeChannelRoomJid: roomSync.activeChannelRoomJid,
    activeThreadStack,
    activeThreadTargetMessageId,
    activeThreadTargetRequestId,
    activeRightPanel,
    threads,
    reactionModeTarget: reactionMode.reactionModeTarget,
    reactionModeState: reactionMode.reactionModeState,
    activeDraft: conversation.activeDraft,
    activeForumTitle: conversation.activeForumTitle,
    activeTypingUsers: conversation.activeTypingUsers,
    contentAreaIsLoadingMessages: conversation.contentAreaIsLoadingMessages,
    activeIsLoadingOlderMessages: conversation.activeIsLoadingOlderMessages,
    activeHasOlderMessages: conversation.activeHasOlderMessages,
    activeIsSending: conversation.activeIsSending,
    activeSearchResults: conversation.activeSearchResults,
    activeIsSearching: conversation.activeIsSearching,
    selfDomain,
    members,
    authorJidByNick: memberDirectory.authorJidByNick,
    mentionCandidates: memberDirectory.mentionCandidates,
    displayedMemberCount: memberDirectory.displayedMemberCount,
    displayedMemberState: memberDirectory.displayedMemberState,
    memberCountLabel: memberDirectory.memberCountLabel,
    activeDmPeer: conversation.activeDmPeer,
    computedChannelUnreadMap,
    groupDmConversations,
    totalTabUnreadCount,
    notifications,
    appUpdate,
    version,
    avatarUrlByAuthor: memberDirectory.avatarUrlByAuthor,
    membersWithAvatars: memberDirectory.membersWithAvatars,
    inferredMemberJids: memberDirectory.inferredMemberJids,
    authorHatsByNick: memberDirectory.authorHatsByNick,
    authorAuthorityByNick: memberDirectory.authorAuthorityByNick,
    activeActionError: conversation.activeActionError,
    activeRoomAccessRequirement: conversation.activeRoomAccessRequirement,
    activeErrorActionLabel: conversation.activeErrorActionLabel,
    activeUploadProgress: conversation.activeUploadProgress,
    setContentAreaRef: conversation.setContentAreaRef,
    getThreadLabel: panels.getThreadLabel,
    refreshAppUpdate,
    handleRequestNotifications: notificationOrchestration.handleRequestNotifications,
    handleToggleNotifications: notificationOrchestration.handleToggleNotifications,
    handleToggleMessageSounds: notificationOrchestration.handleToggleMessageSounds,
    openUserSettings: pageNavigation.openUserSettings,
    openHome: pageNavigation.openHome,
    openDmList: pageNavigation.openDmList,
    openThreads: pageNavigation.openThreads,
    openUnread: pageNavigation.openUnread,
    openCommunitySurface: pageNavigation.openCommunitySurface,
    closeUserSettings: pageNavigation.closeUserSettings,
    handleLogout: lifecycle.handleLogout,
    selectChannel: roomSync.selectChannel,
    selectChannelByRoomJid: roomSync.selectChannelByRoomJid,
    selectGroupDm: roomSync.selectGroupDm,
    onSelectThread: panels.onSelectThread,
    onSelectThreadEntry: panels.onSelectThreadEntry,
    selectExtensionRoute: extension.selectExtensionRoute,
    handleOpenDm: dmSync.handleOpenDm,
    selectDm: dmSync.selectDm,
    handleNewDm: dmSync.handleNewDm,
    handleNewGroupDm: dmSync.handleNewGroupDm,
    handleAddPeopleToDm: dmSync.handleAddPeopleToDm,
    handleCreateGroupDm: dmSync.handleCreateGroupDm,
    openCreateChannelDialog: workspaceDialogs.openCreateChannelDialog,
    handleCreateChannel: workspaceDialogs.handleCreateChannel,
    handleUpdateChannel: workspaceDialogs.handleUpdateChannel,
    handleDeleteChannel: workspaceDialogs.handleDeleteChannel,
    handleMoveChannelToSpace: workspaceDialogs.handleMoveChannelToSpace,
    confirmDeleteChannel: workspaceDialogs.confirmDeleteChannel,
    handleUpdateWaddle: workspaceDialogs.handleUpdateWaddle,
    handleDeleteWaddle: workspaceDialogs.handleDeleteWaddle,
    confirmDeleteWaddle: workspaceDialogs.confirmDeleteWaddle,
    handleRemoveMember: workspaceDialogs.handleRemoveMember,
    confirmRemoveMember: workspaceDialogs.confirmRemoveMember,
    openChannelEdit: workspaceDialogs.openChannelEdit,
    sendActiveMessage: send.sendActiveMessage,
    sendPublicChannelMessage: send.sendPublicChannelMessage,
    sendThreadMessage: send.sendThreadMessage,
    sendCallChatMessage: send.sendCallChatMessage,
    sendGif: send.sendGif,
    openThread: panels.openThread,
    pushThreadFromStack: panels.pushThreadFromStack,
    pushThread: panels.pushThread,
    popThreadTo: panels.popThreadTo,
    closeThreadPanel: panels.closeThreadPanel,
    activateRightPanel: panels.activateRightPanel,
    closeExtensionRoutePanel: panels.closeExtensionRoutePanel,
    closePinnedPanel: panels.closePinnedPanel,
    notifyActiveComposing: send.notifyActiveComposing,
    editActiveMessage: send.editActiveMessage,
    retractActiveMessage: send.retractActiveMessage,
    reactActiveMessage: send.reactActiveMessage,
    pinActiveMessage: pinned.pinActiveMessage,
    unpinActiveMessage: pinned.unpinActiveMessage,
    jumpToPinnedMessage: pinned.jumpToPinnedMessage,
    markActiveDisplayed: send.markActiveDisplayed,
    invokeActiveExtensionAction: send.invokeActiveExtensionAction,
    invokeExtensionRouteAction: send.invokeExtensionRouteAction,
    searchActiveMessages: send.searchActiveMessages,
    clearActiveSearch: send.clearActiveSearch,
    loadOlderActiveMessages: send.loadOlderActiveMessages,
    retryActiveLoad: send.retryActiveLoad,
    ensureActiveMessageLoaded: send.ensureActiveMessageLoaded,
    loadOlderThreadMessages: send.loadOlderThreadMessages,
  };
}

export type ChatAppController = ReturnType<typeof useChatAppController>;
