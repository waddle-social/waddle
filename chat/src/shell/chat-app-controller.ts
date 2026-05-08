import { type ComponentPublicInstance, computed, onMounted, onUnmounted, ref, watch, watchEffect } from "vue";
import { createKeystrok, type Keystrok } from "keystrok";
import { useWaddleDirectory, type MemberLoadState } from "@/waddles/directory";
import { useWaddleMembers } from "@/waddles/members";
import { useDirectMessageConversations } from "@/dms/conversations";
import { useDirectMessages } from "@/dms/messages";
import { useChannelMessages } from "@/channels/messages";
import { useMessageThreads } from "@/channels/threads";
import { useChatShellState } from "@/shell/state";
import { useServiceWorkerUpdate } from "@/shell/service-worker-update";
import { usePushNotifications } from "@/shell/notifications";
import { useChannelInbox } from "@/channels/inbox";
import { useChatReadActivity } from "@/shell/read-activity";
import { useDeploymentVersionInfo } from "@/shell/version";
import { useXmppRosterContacts } from "@/contacts/roster";
import { buildDirectMessagePath, buildChannelExtensionPath, buildChannelPath, buildChatSettingsPath, parseChatLocation, pushDirectMessageRoute, pushChannelExtensionRoute, pushChannelRoute, pushChatSettingsRoute, resolveChannelBySlug, shouldLoadWaddleStructureForRoute } from "@/shell/navigation";
import { barePeerJid, jidDomain, parseManagedRoomBareJid } from "@/lib/xmpp-client";
import { roomJidForChannelId as resolveRoomJidForChannelId } from "@/lib/channel-room";
import { mergeRoomHats, roomHatsFromMembers } from "@/lib/xmpp/occupant-badges";
import { connectionStore } from "@/lib/connection-store";
import { $pinnedRooms } from "@/stores/pinned-messages";
import { orderTimelineForScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import type { MemberSummary } from "@/lib/chat-types";
import type { ExtensionAnnotationAction, MarkupSpan, MessageReference, TimelineMessage } from "@/lib/chat-ui";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import { avatarLookupCandidates, mentionAutocompleteCandidates, mentionMatchesUsername, mergeMentionMembers } from "@/lib/mentions";
import {
  moveReactionSelection,
  preserveReactionSelection,
  quickReactionForKey,
  selectInitialReactionMessage,
  type ReactionModeScope,
} from "@/lib/reaction-mode";

export function useChatAppController(giphyApiKey: string) {
  const ui = useChatShellState();
  const { mode: scrollDirectionMode } = useScrollDirectionPreference();

  const xmppClient = computed(() => connectionStore.client);
  const session = computed(() => connectionStore.session);
  const api = computed(() => connectionStore.api);

  const waddles = useWaddleDirectory(
    api,
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

  const dmConversations = useDirectMessageConversations(
    session,
    xmppClient,
  );

  const channelUnread = useChannelInbox(xmppClient);
  const rosterContacts = useXmppRosterContacts(xmppClient);

  const dmMessaging = useDirectMessages(
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
    scrollToMessage: (messageId: string) => Promise<void>;
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
  const extensionRoutes = ref<DiscoveredExtensionRoute[]>([]);
  const activeExtensionRouteKey = ref<{ channelId: string; pluginId: string; routeId: string } | null>(null);
  const activeExtensionRoute = computed(() => {
    const key = activeExtensionRouteKey.value;
    if (!key) return null;
    return extensionRoutes.value.find((route) =>
      route.pluginId === key.pluginId && route.routeId === key.routeId,
    ) ?? null;
  });
  const activeChannelRoomJid = computed(() => {
    const channel = waddles.currentChannel.value;
    if (!channel || !session.value) return null;
    return channel.jid ?? resolveRoomJidForChannelId(session.value, waddles.channels.value, channel.id);
  });

  // Thread panel state - stack = breadcrumb trail into nested sub-threads.
  // Empty stack = panel closed. Only meaningful for channel messages since DMs
  // don't use XEP-0201 threads yet.
  const activeThreadStack = ref<string[]>([]);
  const activeThreadTargetMessageId = ref<string | null>(null);
  const threads = useMessageThreads(activeMessages);
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
  const isApplyingRoute = ref(false);
  let routeRequestId = 0;
  const activeIsLoadingMessages = computed(() =>
    ui.sidebarMode.value === "dms" ? dmMessaging.isLoadingMessages.value : messaging.isLoadingMessages.value,
  );
  const isResolvingActiveConversation = computed(() =>
    ui.activePage.value === "chat"
    && !waddles.currentChannel.value
    && !activeDmPeer.value
    && (isApplyingRoute.value || waddles.isLoadingStructure.value),
  );
  const contentAreaIsLoadingMessages = computed(() =>
    activeIsLoadingMessages.value || isResolvingActiveConversation.value,
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
  const mentionCandidates = computed(() =>
    mentionAutocompleteCandidates(mergedMentionMembers.value.members).map((candidate) => {
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
  watch(mentionSourceDiagnostic, (detail) => {
    if (detail) console.warn(detail);
  });
  const memberRoleOrder = { owner: 0, admin: 1, member: 2, outcast: 3, none: 4 } as const;
  const displayedMembers = computed<MemberSummary[]>(() =>
    [...mergedMentionMembers.value.members].sort(
      (a, b) =>
        (memberRoleOrder[a.role] ?? 4) - (memberRoleOrder[b.role] ?? 4) ||
        a.username.localeCompare(b.username, undefined, { sensitivity: "base" }),
    ),
  );
  const authoritativeMemberJids = computed(() => new Set(waddles.members.value.map((member) => member.jid)));
  const inferredMemberJids = computed(() =>
    new Set(displayedMembers.value
      .filter((member) => !authoritativeMemberJids.value.has(member.jid))
      .map((member) => member.jid)),
  );
  const displayedMemberCount = computed<number | null>(() => {
    const count = displayedMembers.value.length;
    if (count > 0) return count;
    return waddles.memberLoadState.value === "ready" ? 0 : null;
  });
  const displayedMemberState = computed<MemberLoadState>(() => waddles.memberLoadState.value);
  const memberCountLabel = computed(() => {
    if (displayedMemberCount.value !== null) return String(displayedMemberCount.value);
    if (displayedMemberState.value === "loading") return "syncing";
    if (displayedMemberState.value === "unavailable") return "unavailable";
    return "0";
  });
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

  const activeTarget = computed(() =>
    ui.sidebarMode.value === "dms" ? dmMessaging : messaging,
  );
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
    activeTarget,
    roomJidForChannelId: resolveRoomJidForChannelId,
  });

  const notifications = usePushNotifications();
  const appUpdate = useServiceWorkerUpdate();
  const version = useDeploymentVersionInfo(xmppClient);
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
    displayedMembers.value.map((member) => ({
      ...member,
      avatar_url: fetchedAvatarUrlByJid.value[member.jid] ?? member.avatar_url,
    })),
  );
  const authorHatsByNick = computed(() =>
    mergeRoomHats(roomHatsFromMembers(displayedMembers.value), messaging.roomHats.value),
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
    // (rooms vs DMs), so calling both is idempotent - whichever owns the id
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
      // Only re-hydrate inbox on resumptions (reconnect after brief disconnect).
      // Fresh connections are handled by onConnectionReady() after appState -> "ready".
      if (event.type === "resumed") {
        void dmConversations.hydrateFromInbox();
        void channelUnread.hydrateFromInbox();
      }
    });
  }, { immediate: true });

  const settingsPath = buildChatSettingsPath();

  const currentChatPath = computed(() =>
    ui.activePage.value === "extension" && activeExtensionRouteKey.value
      ? buildChannelExtensionPath(
          waddles.currentChannel.value,
          activeExtensionRouteKey.value.pluginId,
          activeExtensionRouteKey.value.routeId,
        )
      : ui.sidebarMode.value === "dms" && activeDmPeer.value
      ? buildDirectMessagePath(activeDmPeer.value.peerUsername)
      : buildChannelPath(
          waddles.currentChannel.value,
          activeThreadStack.value,
          ui.showPinnedPanel.value,
        ),
  );

  async function setupPushSubscription() {
    if (!xmppClient.value || !connectionStore.session) return;
    await notifications.syncPushSubscription(xmppClient.value, connectionStore.session.jid);
  }

  async function refreshExtensionRoutes() {
    if (!xmppClient.value) {
      extensionRoutes.value = [];
      return;
    }
    try {
      extensionRoutes.value = await xmppClient.value.discoverExtensionRoutes();
    } catch (error) {
      console.warn("Unable to discover extension routes", error);
      extensionRoutes.value = [];
    }
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

  const activeUploadProgress = computed(() =>
    ui.sidebarMode.value === "dms" ? dmMessaging.uploadProgress.value : messaging.uploadProgress.value,
  );
  const activeActionError = computed(() => ui.actionError.value);
  const activeErrorActionLabel = computed(() => {
    const peer = activeDmPeer.value;
    return ui.sidebarMode.value === "dms" &&
      peer &&
      dmMessaging.loadErrorPeerJid.value === peer.peerJid &&
      activeActionError.value === dmMessaging.loadErrorMessage.value
      ? "Try again"
      : null;
  });
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

  function openThread(threadId: string, targetMessageId?: string) {
    if (!threadId) return;
    activeThreadTargetMessageId.value = targetMessageId ?? null;
    if (
      activeThreadStack.value.length > 0 &&
      activeThreadStack.value[activeThreadStack.value.length - 1] === threadId
    ) {
      if (targetMessageId) void messaging.backfillThread(threadId);
      return;
    }
    activeThreadStack.value = [threadId];
    void messaging.backfillThread(threadId);
  }

  function roomJidForChannelId(channelId: string): string | null {
    const sess = connectionStore.session;
    if (!sess) return null;
    return resolveRoomJidForChannelId(sess, waddles.channels.value, channelId);
  }

  async function onSelectThread(channelId: string, threadId: string) {
    // Navigate to the channel if not already there
    if (waddles.activeChannelId.value !== channelId) {
      await selectChannel(channelId);
    }
    // Mark thread as read
    const roomJid = roomJidForChannelId(channelId);
    if (roomJid) {
      channelUnread.markThreadRead(roomJid, threadId);
    }
    openThread(threadId);
  }

  function pushThread(threadId: string) {
    if (!threadId) return;
    activeThreadTargetMessageId.value = null;
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
    activeThreadTargetMessageId.value = null;
    if (index < 0) {
      activeThreadStack.value = [];
      return;
    }
    activeThreadStack.value = activeThreadStack.value.slice(0, index + 1);
  }

  function closeThreadPanel() {
    activeThreadTargetMessageId.value = null;
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

  /** #414: pin or unpin the targeted message in the active channel. The
   * server gates on Owner/Admin affiliation; the action sheet entry is
   * also visibility-gated client-side, so non-admins shouldn't reach
   * this — but the server is authoritative. */
  function pinActiveMessage(messageId: string) {
    const client = xmppClient.value;
    const channel = waddles.currentChannel.value;
    const space = waddles.currentSpace.value;
    if (!client || !channel || !space) return;
    const stanzaId = resolvePinTargetStanzaId(messageId);
    if (!stanzaId) return;
    void client.pinMessage(space.id, channel.id, stanzaId).catch((error: unknown) => {
      console.warn("pinMessage failed", error);
    });
  }

  function unpinActiveMessage(messageId: string) {
    const client = xmppClient.value;
    const channel = waddles.currentChannel.value;
    const space = waddles.currentSpace.value;
    if (!client || !channel || !space) return;
    const stanzaId = resolvePinTargetStanzaId(messageId);
    if (!stanzaId) return;
    void client.unpinMessage(space.id, channel.id, stanzaId).catch((error: unknown) => {
      console.warn("unpinMessage failed", error);
    });
  }

  /** #414: jump to a pinned message from the panel — load it into the
   * timeline if needed, then scroll/center. Stanza-id is the room
   * archive id; message-id used in the timeline matches via wireIds /
   * reactionTargetId. The chat client's existing
   * `scrollToMessage(messageId)` accepts the wire id; we route the
   * stanza-id directly since `ensureMessageLoaded` resolves both. */
  async function jumpToPinnedMessage(stanzaId: string) {
    await ensureActiveMessageLoaded(stanzaId);
    await contentAreaRef.value?.scrollToMessage(stanzaId);
  }

  /** Map a chat-side message id to the room's XEP-0359 stanza-id. The
   * pin server expects the stable archive id stamped by-room, not the
   * wire `id` attribute or the client-assigned origin-id. Timeline
   * rows expose this as `reactionTargetId` (room messages) /
   * `replyableId` (DMs use this for reply-to); both pull from
   * `message.stanza_id` upstream. Returns null when no archive id is
   * known yet (e.g., a queued send hasn't been reflected). */
  function resolvePinTargetStanzaId(messageId: string): string | null {
    const message = messaging.messages.value.find((m) => m.id === messageId);
    if (!message) return null;
    const m = message as TimelineMessage & {
      reactionTargetId?: string;
      replyableId?: string;
    };
    return m.reactionTargetId ?? m.replyableId ?? null;
  }

  function markActiveDisplayed(messageId: string) {
    activeTarget.value.markDisplayed(messageId);
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
    if (ui.sidebarMode.value !== "dms" || !peer) return;
    void dmMessaging.loadMessages(peer.peerJid);
  }

  function ensureActiveMessageLoaded(messageId: string) {
    return activeTarget.value.ensureMessageLoaded(messageId);
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
      pushChatSettingsRoute("app");
      return;
    }
    if (ui.activePage.value === "dashboard") {
      pushChannelRoute(null);
      return;
    }
    if (ui.activePage.value === "extension") {
      pushChannelExtensionRoute(
        waddles.currentChannel.value,
        activeExtensionRouteKey.value?.pluginId,
        activeExtensionRouteKey.value?.routeId,
      );
      return;
    }
    if (ui.sidebarMode.value === "dms" && activeDmPeer.value) {
      pushDirectMessageRoute(activeDmPeer.value.peerUsername);
    } else {
      pushChannelRoute(
        waddles.currentChannel.value,
        activeThreadStack.value,
        ui.showPinnedPanel.value,
      );
    }
  }

  watch(
    [waddles.activeChannelId, ui.sidebarMode, () => dmConversations.activePeerJid.value],
    () => {
      // Channel / DM / mode changes close any open thread panel - the ids inside
      // the stack belong to the channel we just left.
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      // #414: pin panel state is per-room — clear on channel switch.
      ui.showPinnedPanel.value = false;
      exitReactionMode();
      updateUrl();
    },
  );

  watch(activeThreadStack, () => {
    exitReactionMode();
    updateUrl();
  }, { deep: true });
  // #414: any toggle of the pin panel pushes the URL state.
  watch(() => ui.showPinnedPanel.value, () => {
    updateUrl();
  });
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
    ui.activePage.value = activeExtensionRouteKey.value && waddles.currentChannel.value
      ? "extension"
      : waddles.currentChannel.value || activeDmPeer.value
        ? "chat"
        : "dashboard";
    if (window.location.pathname + window.location.search !== currentChatPath.value) {
      if (ui.activePage.value === "extension") {
        pushChannelExtensionRoute(
          waddles.currentChannel.value,
          activeExtensionRouteKey.value?.pluginId,
          activeExtensionRouteKey.value?.routeId,
        );
      } else if (ui.sidebarMode.value === "dms" && activeDmPeer.value) {
        pushDirectMessageRoute(activeDmPeer.value.peerUsername);
      } else {
        pushChannelRoute(waddles.currentChannel.value, activeThreadStack.value);
      }
    }
  }

  function onPopState() {
    const route = parseChatLocation(window.location.pathname, window.location.search);
    ui.activePage.value = route.page;
    if (route.page === "settings") {
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      return;
    }
    if (route.page === "dashboard") {
      ui.sidebarMode.value = "channels";
      dmConversations.closeDm();
      waddles.activeChannelId.value = null;
      activeExtensionRouteKey.value = null;
      messaging.clearMessages();
      activeThreadTargetMessageId.value = null;
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

  async function applyRouteTarget(route: ReturnType<typeof parseChatLocation>, requestId: number) {
    ui.activePage.value = route.page;
    // #414: sync the pin panel toggle with `?pinned=1`. Channel-only;
    // dashboard/settings/extension/DM routes leave it false.
    ui.showPinnedPanel.value = route.pinnedPanelOpen && route.page === "chat" && !route.dmUsername;
    if (route.page === "settings") {
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      return;
    }
    if (route.page === "dashboard") {
      ui.sidebarMode.value = "channels";
      dmConversations.closeDm();
      waddles.activeChannelId.value = null;
      activeExtensionRouteKey.value = null;
      messaging.clearMessages();
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      if (shouldLoadWaddleStructureForRoute(route, waddles.channels.value.length)) {
        await waddles.loadStructure(null, { noChannelSelect: true });
      }
      return;
    }
    if (route.dmUsername) {
      activeExtensionRouteKey.value = null;
      const username = route.dmUsername.replace(/^@/, "").trim();
      if (username) {
        const domain = session.value ? jidDomain(session.value.jid) : "";
        if (!domain) return;
        await handleOpenDm(`${username}@${domain}`);
      }
      return;
    }

    if (shouldLoadWaddleStructureForRoute(route, waddles.channels.value.length)) {
      await waddles.loadStructure();
      if (requestId !== routeRequestId) return;
    }

    if (route.page === "extension") {
      ui.sidebarMode.value = "channels";
      dmConversations.closeDm();
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      if (route.channelSlug) {
        const ch = resolveChannelBySlug(route.channelSlug, waddles.channels.value);
        if (!ch) {
          waddles.activeChannelId.value = null;
          activeExtensionRouteKey.value = null;
          return;
        }
        waddles.activeChannelId.value = ch.id;
        void waddles.reloadChannelMembers(ch.id);
        activeExtensionRouteKey.value = route.extensionPluginId && route.extensionRouteId
          ? { channelId: ch.id, pluginId: route.extensionPluginId, routeId: route.extensionRouteId }
          : null;
      }
      return;
    }

    if (route.channelSlug) {
      activeExtensionRouteKey.value = null;
      const ch = resolveChannelBySlug(route.channelSlug, waddles.channels.value);
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
    activeThreadTargetMessageId.value = null;
    activeThreadStack.value = route.threadStack;
    for (const threadId of route.threadStack) {
      void messaging.backfillThread(threadId);
    }
  }

  // --- Bootstrap (watches connection store) ---

  async function onConnectionReady() {
    const route = parseChatLocation(window.location.pathname, window.location.search);
    const requestId = ++routeRequestId;
    isApplyingRoute.value = true;

    try {
      if (route.page === "dashboard") {
        await waddles.loadStructure(null, { noChannelSelect: true });
      } else {
        await waddles.loadSpace({ loadStructure: !shouldLoadWaddleStructureForRoute(route, waddles.channels.value.length) });
      }
      await refreshExtensionRoutes();
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
    extensionRoutes.value = [];
    activeExtensionRouteKey.value = null;
    setupPromptShown = false;
    messaging.clearMessages();
    dmMessaging.clearMessages();
    // #414: drop all pin state on logout so a subsequent login doesn't
    // see the prior user's pinned-message previews.
    $pinnedRooms.set(new Map());
    ui.showPinnedPanel.value = false;
    pushChannelRoute(null);
    await connectionStore.logout();
  }

  async function selectChannel(channelId: string) {
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "channels";
    activeExtensionRouteKey.value = null;
    dmConversations.closeDm();
    memberJidByNick.value = {};
    waddles.activeChannelId.value = channelId;
    void waddles.reloadChannelMembers(channelId);
    messaging.clearMessages();
    // XEP-0502: Clear activity indicator for this channel
    const roomJid = roomJidForChannelId(channelId);
    if (roomJid) {
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

  async function selectExtensionRoute(channelId: string, route: DiscoveredExtensionRoute) {
    ui.activePage.value = "extension";
    ui.sidebarMode.value = "channels";
    dmConversations.closeDm();
    activeThreadTargetMessageId.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = { channelId, pluginId: route.pluginId, routeId: route.routeId };
    memberJidByNick.value = {};
    waddles.activeChannelId.value = channelId;
    void waddles.reloadChannelMembers(channelId);
    ui.showMobileNav.value = false;
  }

  async function handleOpenDm(peerJid: string) {
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "dms";
    activeExtensionRouteKey.value = null;
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
      // Space-only: topology reloaded, no room to load - leave channel unselected.
      return;
    }
    // MUC was created (muc / space-muc / space-with-muc): select and load messages.
    ui.activePage.value = "chat";
    activeExtensionRouteKey.value = null;
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
    return {
      connectionStore,
      giphyApiKey,
      ui,
      waddles,
      messaging,
      dmConversations,
      channelUnread,
      rosterContacts,
      dmMessaging,
      xmppClient,
      activeMessages,
      activeFirstUnseenId,
      extensionRoutes,
      activeExtensionRouteKey,
      activeExtensionRoute,
      activeChannelRoomJid,
      activeThreadStack,
      activeThreadTargetMessageId,
      threads,
      reactionModeTarget,
      reactionModeState,
      activeDraft,
      activeForumTitle,
      activeTypingUsers,
      contentAreaIsLoadingMessages,
      activeIsLoadingOlderMessages,
      activeHasOlderMessages,
      activeIsSending,
      activeSearchResults,
      activeIsSearching,
      selfDomain,
      members,
      authorJidByNick,
      mentionCandidates,
      displayedMemberCount,
      displayedMemberState,
      memberCountLabel,
      activeDmPeer,
      computedChannelUnreadMap,
      totalTabUnreadCount,
      notifications,
      appUpdate,
      version,
      avatarUrlByAuthor,
      membersWithAvatars,
      inferredMemberJids,
      authorHatsByNick,
      activeActionError,
      activeErrorActionLabel,
      activeUploadProgress,
      setContentAreaRef,
      getThreadLabel,
      refreshAppUpdate,
      handleRequestNotifications,
      handleToggleNotifications,
      openUserSettings,
      closeUserSettings,
      handleLogout,
      selectChannel,
      onSelectThread,
      selectExtensionRoute,
      handleOpenDm,
      selectDm,
      handleNewDm,
      openCreateChannelDialog,
      handleCreateChannel,
      handleUpdateChannel,
      handleDeleteChannel,
      confirmDeleteChannel,
      handleUpdateWaddle,
      handleDeleteWaddle,
      confirmDeleteWaddle,
      handleRemoveMember,
      confirmRemoveMember,
      openChannelEdit,
      sendActiveMessage,
      sendThreadMessage,
      sendGif,
      openThread,
      pushThread,
      popThreadTo,
      closeThreadPanel,
      notifyActiveComposing,
      editActiveMessage,
      retractActiveMessage,
      reactActiveMessage,
      pinActiveMessage,
      unpinActiveMessage,
      jumpToPinnedMessage,
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

export type ChatAppController = ReturnType<typeof useChatAppController>;
