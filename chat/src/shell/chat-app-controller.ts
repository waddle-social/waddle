import { type ComponentPublicInstance, computed, onMounted, onUnmounted, ref, watch, watchEffect } from "vue";
import { createKeystrok, type Keystrok } from "keystrok";
import { useStore } from "@nanostores/vue";
import { useWaddleDirectory, type MemberLoadState } from "@/waddles/directory";
import { useWaddleMembers } from "@/waddles/members";
import { useDirectMessageConversations } from "@/dms/conversations";
import { useDirectMessages } from "@/dms/messages";
import { useChannelMessages } from "@/channels/messages";
import { useMessageThreads } from "@/channels/threads";
import { useChatShellState } from "@/shell/state";
import { useServiceWorkerUpdate } from "@/shell/service-worker-update";
import { shouldShowChannelForegroundNotification, usePushNotifications } from "@/shell/notifications";
import { createBrowserMessageTonePlayer } from "@/shell/audio-alerts";
import { useChatWindowVisibility } from "@/shell/window-visibility";
import { useChannelInbox } from "@/channels/inbox";
import { useSocialFeed } from "@/services/social-feed";
import { useStories } from "@/services/stories";
import { useCommunityEvents } from "@/services/community-events";
import { useChatReadActivity } from "@/shell/read-activity";
import {
  shouldPreserveActiveChannelDuringStructureRetry,
  shouldRetryMissingStructureLoad,
} from "@/shell/structure-retry";
import { useDeploymentVersionInfo } from "@/shell/version";
import { useXmppRosterContacts } from "@/contacts/roster";
import { matchLocation, navigate, type RouteMatch } from "@/router";
import { resolveChannelBySlug } from "@/shell/route-helpers";
import { barePeerJid, jidDomain, parseManagedRoomBareJid, type LiveDmMessage, type RoomActivityEvent } from "@/lib/xmpp-client";
import { createNotifySettingsStore, type NotifySettingsStore } from "@/lib/notify-settings";
import { mdsChatKey, queueMdsDisplayed, setMdsDisplayed } from "@/lib/last-seen-store";
import {
  isTrustedManagedRoomJid,
  knownChannelIdForRoomJid,
  roomJidForChannelId as resolveRoomJidForChannelId,
} from "@/lib/channel-room";
import { normalizeMucServiceDomain } from "@/lib/calls/muc-call-indicators";
import { resolveThreadEntryTarget } from "@/lib/threads-view-target";
import { connectionStore } from "@/lib/connection-store";
import { $pinnedRooms, hydratePinnedRoom, pinnedRoomsEpoch, resetPinnedRooms } from "@/stores/pinned-messages";
import { hydratePinnedBodiesOnPanelOpen, hydrateSinglePinnedBody } from "@/services/pinned-message-bodies";
import { trustedLinkPreviewMediaOrigin } from "@/lib/xmpp/link-preview";
import { dmMessageFromArchived, roomMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import { mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import { fromLiveDmMessage } from "@/dms/message-timeline-state";
import { orderTimelineForScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import type { MemberSummary } from "@/lib/chat-types";
import type { ExtensionAnnotationAction, MarkupSpan, MessageReference, TimelineMessage } from "@/lib/chat-ui";
import type { ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import { avatarLookupCandidatesAcrossContexts, mentionAutocompleteCandidates, mentionMatchesBareJid, mergeMentionMembers } from "@/lib/mentions";
import {
  moveReactionSelection,
  preserveReactionSelection,
  quickReactionForKey,
  selectInitialReactionMessage,
  type ReactionModeScope,
} from "@/lib/reaction-mode";

interface ChannelActivityNotificationDeps {
  notifySettings: Pick<NotifySettingsStore, "getMode">;
  notifications: {
    showMentionNotification: (opts: {
      senderNick: string;
      channelName: string;
      body: string;
      roomJid: string;
      isBroadcast: boolean;
      stanzaId?: string;
      onNavigate?: (roomJid: string) => void;
    }) => void;
    showChannelMessageNotification: (opts: {
      senderNick: string;
      channelName: string;
      body: string;
      roomJid: string;
      stanzaId?: string;
      onNavigate?: (roomJid: string) => void;
    }) => void;
  };
  messageSound?: {
    play: (key: string) => void | Promise<void>;
  };
  messageSoundsEnabled?: () => boolean;
  canShowForegroundNotification?: () => boolean;
  isDoNotDisturb?: () => boolean;
  isTabFocused?: () => boolean;
  sessionJid: string | null | undefined;
  resolveChannelNameFromJid: (roomJid: string) => string | null;
  onNavigate: (roomJid: string) => void;
}

interface DmActivityNotificationDeps {
  notifySettings: Pick<NotifySettingsStore, "getMode">;
  notifications: {
    showDmNotification: (opts: {
      senderUsername: string;
      peerJid: string;
      body: string;
      stanzaId?: string;
      onNavigate?: (peerJid: string) => void;
    }) => void;
  };
  messageSound?: {
    play: (key: string) => void | Promise<void>;
  };
  messageSoundsEnabled?: () => boolean;
  canShowForegroundNotification?: () => boolean;
  isDoNotDisturb?: () => boolean;
  isTabFocused?: () => boolean;
  sessionJid: string | null | undefined;
  activePeerJid: string | null | undefined;
  onNavigate: (peerJid: string) => void;
}

export function showForegroundNotificationForChannelActivity(
  event: RoomActivityEvent,
  deps: ChannelActivityNotificationDeps,
): void {
  const channelName = deps.resolveChannelNameFromJid(event.roomJid) ?? "unknown";
  const isBroadcast = !!event.broadcastMention;
  const isPersonalMention = event.mentions?.some((mention) =>
    mentionMatchesBareJid(mention, deps.sessionJid)
  ) ?? false;
  const isMention = isBroadcast || isPersonalMention;
  const mode = deps.notifySettings.getMode(event.roomJid, "private-group");

  if (!shouldShowChannelForegroundNotification({ mode, isMention })) return;
  if (deps.isDoNotDisturb?.() === true) return;
  if (deps.canShowForegroundNotification?.() === false) return;

  if (deps.isTabFocused?.() === false && deps.messageSoundsEnabled?.() !== false) {
    void deps.messageSound?.play(messageSoundKey(event.roomJid, event.stanzaId));
  }

  if (isMention) {
    deps.notifications.showMentionNotification({
      senderNick: event.nick,
      channelName,
      body: event.body,
      roomJid: event.roomJid,
      isBroadcast,
      stanzaId: event.stanzaId,
      onNavigate: deps.onNavigate,
    });
    return;
  }

  deps.notifications.showChannelMessageNotification({
    senderNick: event.nick,
    channelName,
    body: event.body,
    roomJid: event.roomJid,
    stanzaId: event.stanzaId,
    onNavigate: deps.onNavigate,
  });
}

export function showForegroundNotificationsForChannelActivities(
  events: RoomActivityEvent[],
  deps: ChannelActivityNotificationDeps,
): void {
  for (const event of events) {
    showForegroundNotificationForChannelActivity(event, deps);
  }
}

export function showForegroundNotificationForDmActivity(
  message: LiveDmMessage,
  deps: DmActivityNotificationDeps,
): void {
  const isSelf = barePeerJid(message.fromJid) === barePeerJid(deps.sessionJid ?? "");
  const isViewingThisDm = deps.activePeerJid === message.peerJid;
  if (isSelf || isViewingThisDm) return;

  const mode = deps.notifySettings.getMode(message.peerJid, "direct-chat");
  const isMention = message.mentions?.some((mention) =>
    mentionMatchesBareJid(mention, deps.sessionJid)
  ) ?? false;
  if (!shouldShowChannelForegroundNotification({ mode, isMention })) return;
  if (deps.isDoNotDisturb?.() === true) return;
  if (deps.canShowForegroundNotification?.() === false) return;

  if (deps.isTabFocused?.() === false && deps.messageSoundsEnabled?.() !== false) {
    void deps.messageSound?.play(messageSoundKey(message.peerJid, message.stanzaId ?? message.id));
  }

  deps.notifications.showDmNotification({
    senderUsername: message.nick,
    peerJid: message.peerJid,
    body: message.body,
    stanzaId: message.stanzaId,
    onNavigate: deps.onNavigate,
  });
}

function messageSoundKey(conversationJid: string, stanzaId: string | undefined): string {
  return `message:${conversationJid}:${stanzaId ?? createUnstampedMessageSoundId()}`;
}

function createUnstampedMessageSoundId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `unstamped-${crypto.randomUUID()}`;
  }
  return `unstamped-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export function useChatAppController(giphyApiKey: string) {
  const ui = useChatShellState();
  const { mode: scrollDirectionMode } = useScrollDirectionPreference();
  const { isWindowFocused } = useChatWindowVisibility();

  // Seed page-level UI state from the current URL so the first render
  // lands on the right page. Without this, every cold load flashes the
  // default home dashboard / channels-sidebar for one tick before
  // `onConnectionReady` runs `applyRouteTarget` and snaps to the real
  // page. `applyMatchToShellState` is a hoisted function declaration
  // defined further down in this scope.
  if (typeof window !== "undefined") {
    applyMatchToShellState(matchLocation(window.location.pathname, window.location.search));
  }

  const xmppClient = computed(() => connectionStore.client);
  const session = computed(() => connectionStore.session);
  const api = computed(() => connectionStore.api);
  // Per-controller XEP-0492 store. Constructed here (not a
  // module-level singleton) so unrelated consumers can't share
  // state implicitly — PR-review compliance. Exposed on the
  // controller return and threaded into child components via
  // explicit props.
  const notifySettings = createNotifySettingsStore();

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
    const domain = jid.split("@")[1]?.split("/")[0];
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
  );
  const pinnedRooms = useStore($pinnedRooms);

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
  function isActiveDirectDmSurface(): boolean {
    return ui.sidebarMode.value === "dms" && !!dmConversations.activePeerJid.value;
  }

  watchEffect(() => {
    const timeline = contentAreaRef.value?.messagesContainer ?? null;
    const edgeScroller = contentAreaRef.value?.scrollToPinnedEdge ?? null;
    if (isActiveDirectDmSurface()) {
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
    isActiveDirectDmSurface() ? dmMessaging.messages.value : messaging.messages.value,
  );
  const activeFirstUnseenId = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.firstUnseenId.value : messaging.firstUnseenId.value,
  );
  const extensionRoutes = ref<DiscoveredExtensionRoute[]>([]);
  const selectedChannelRoomJids = ref<Record<string, string>>({});
  const selfDomain = computed(() => (session.value ? jidDomain(session.value.jid) : ""));
  const managedMucDomain = computed(() =>
    normalizeMucServiceDomain(waddles.mucServiceJid.value) || (selfDomain.value ? `muc.${selfDomain.value}` : ""),
  );
  const pendingChannelRoomJidSelection = ref<string | null>(null);
  function clearPendingChannelRoomJidSelection() {
    pendingChannelRoomJidSelection.value = null;
  }
  const activeExtensionRouteKey = ref<{ channelId: string; pluginId: string; routeId: string } | null>(null);
  const activeExtensionRoute = computed(() => {
    const key = activeExtensionRouteKey.value;
    if (!key) return null;
    return extensionRoutes.value.find((route) =>
      route.pluginId === key.pluginId && route.routeId === key.routeId,
    ) ?? null;
  });
  const channelExtensionRoutes = computed(() =>
    ui.sidebarMode.value === "channels" && waddles.currentChannel.value
      ? extensionRoutes.value.filter((route) => route.scope === "channel")
      : [],
  );
  const activeChannelRoomJid = computed(() => {
    const channelId = waddles.activeChannelId.value;
    const channel = waddles.currentChannel.value;
    if (!channelId || !session.value) return null;
    if (channel?.jid) return barePeerJid(channel.jid);
    const selectedRoomJid = selectedChannelRoomJids.value[channelId];
    if (selectedRoomJid) return selectedRoomJid;
    return resolveRoomJidForChannelId(session.value, waddles.channels.value, channelId);
  });
  watch(
    [activeChannelRoomJid, () => waddles.channels.value],
    ([roomJid]) => {
      if (ui.sidebarMode.value !== "channels" || !roomJid) return;
      const normalizedRoomJid = barePeerJid(roomJid);
      const channel = waddles.channels.value.find((candidate) =>
        candidate.jid ? barePeerJid(candidate.jid) === normalizedRoomJid : false
      );
      if (!channel || channel.id === waddles.activeChannelId.value) return;
      void selectChannel(channel.id, { roomJid: normalizedRoomJid });
    },
  );
  watch(
    [() => waddles.channels.value, managedMucDomain],
    () => {
      const roomJid = pendingChannelRoomJidSelection.value;
      if (!roomJid) return;
      const channelId = knownChannelIdForRoomJid(
        roomJid,
        waddles.channels.value,
        managedMucDomain.value,
      );
      if (!channelId) return;
      pendingChannelRoomJidSelection.value = null;
      void selectChannel(channelId, { roomJid });
    },
  );

  // Thread panel state - stack = breadcrumb trail into nested sub-threads.
  // Empty stack = panel closed. Channels and DMs both use XEP-0201 threads.
  const activeThreadStack = ref<string[]>([]);
  const activeThreadTargetMessageId = ref<string | null>(null);
  const threads = useMessageThreads(activeMessages);
  type ActiveRightPanel = "thread" | "pinned" | "extension";
  const activeRightPanel = ref<ActiveRightPanel | null>(null);
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

  function isRightPanelAvailable(panel: ActiveRightPanel): boolean {
    if (ui.activePage.value !== "chat") return false;
    if (panel === "thread") return activeThreadStack.value.length > 0;
    if (panel === "pinned") {
      if (!ui.showPinnedPanel.value) return false;
      if (ui.sidebarMode.value === "dms") return !!dmConversations.activePeerJid.value;
      return ui.sidebarMode.value === "channels" && !!waddles.currentChannel.value && !!activeChannelRoomJid.value;
    }
    if (ui.sidebarMode.value !== "channels") return false;
    return !!activeExtensionRouteKey.value && !!waddles.currentChannel.value;
  }

  function bestAvailableRightPanel(exclude?: ActiveRightPanel): ActiveRightPanel | null {
    const candidates: ActiveRightPanel[] = ["thread", "pinned", "extension"];
    return candidates.find((candidate) => candidate !== exclude && isRightPanelAvailable(candidate)) ?? null;
  }

  function activateRightPanel(panel: ActiveRightPanel) {
    if (isRightPanelAvailable(panel)) activeRightPanel.value = panel;
  }

  function normalizeActiveRightPanel(exclude?: ActiveRightPanel) {
    if (activeRightPanel.value && activeRightPanel.value !== exclude && isRightPanelAvailable(activeRightPanel.value)) return;
    activeRightPanel.value = bestAvailableRightPanel(exclude);
  }

  function closeExtensionRoutePanel() {
    activeExtensionRouteKey.value = null;
    normalizeActiveRightPanel("extension");
  }

  function closePinnedPanel() {
    ui.showPinnedPanel.value = false;
    normalizeActiveRightPanel("pinned");
  }

  const orderedMainReactionMessages = computed(() =>
    orderTimelineForScrollDirection(
      reactionModeMessageCandidates(activeMessages.value.filter((message) => !message.threadId || message.id === message.threadId)),
      scrollDirectionMode.value,
    ),
  );

  const activeThreadReactionMessages = computed<TimelineMessage[]>(() => {
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
    if (ui.sidebarMode.value === "channels" && activeRightPanel.value === "thread" && activeThreadStack.value.length > 0) return "thread";
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
    get: () => (isActiveDirectDmSurface() ? dmMessaging.draft.value : messaging.draft.value),
    set: (value: string) => {
      if (isActiveDirectDmSurface()) dmMessaging.draft.value = value;
      else messaging.draft.value = value;
    },
  });
  const activeForumTitle = computed({
    get: () => (isActiveDirectDmSurface() ? "" : messaging.forumPostTitle.value),
    set: (value: string) => {
      if (!isActiveDirectDmSurface()) {
        messaging.forumPostTitle.value = value;
      }
    },
  });
  const activeTypingUsers = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.typingUsers.value : messaging.typingUsers.value,
  );
  const isApplyingRoute = ref(false);
  let routeRequestId = 0;
  const activeIsLoadingMessages = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.isLoadingMessages.value : messaging.isLoadingMessages.value,
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
    isActiveDirectDmSurface() ? dmMessaging.isLoadingOlderMessages.value : messaging.isLoadingOlderMessages.value,
  );
  const activeHasOlderMessages = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.hasOlderMessages.value : messaging.hasOlderMessages.value,
  );
  const activeIsSending = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.isSending.value : messaging.isSending.value,
  );
  const activeSearchResults = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.searchResults.value : messaging.searchResults.value,
  );
  const activeIsSearching = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.isSearching.value : messaging.isSearching.value,
  );

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
  const memberAffiliationOrder = { owner: 0, admin: 1, member: 2, outcast: 3, none: 4 } as const;
  const displayedMembers = computed<MemberSummary[]>(() =>
    [...mergedMentionMembers.value.members].sort(
      (a, b) =>
        (memberAffiliationOrder[a.affiliation] ?? 4) - (memberAffiliationOrder[b.affiliation] ?? 4) ||
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
    isActiveDirectDmSurface() ? dmMessaging : messaging,
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
  const selfPresenceShow = ref<"available" | "away" | "xa" | "dnd" | "offline">("available");
  const appUpdate = useServiceWorkerUpdate();
  const version = useDeploymentVersionInfo(xmppClient);
  // RFC 363 PR 6: include DM peers in the iterate-and-pull avatar
  // candidate set. Without this, avatars in DM views never resolve
  // (the peer never appears in `messaging.messages` because that's
  // the channel-message timeline). After #435 (workspace roster
  // bridge) lands, push delivery covers both axes and this fallback
  // can be slimmed back down.
  const avatarCandidates = computed(() =>
    avatarLookupCandidatesAcrossContexts({
      channelMembers: mergedMentionMembers.value.members,
      channelMessages: messaging.messages.value,
      channelAuthorJidByNick: authorJidByNick.value,
      dmMessages: dmMessaging.messages.value,
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
  // XEP-0317 hats are server-emitted descriptive metadata only.
  // No client-side fabrication: owner / admin / moderator state
  // flows separately as `authorAuthorityByNick` below (XEP-0045
  // affiliation + role), and the UI renders the two layers
  // independently in MessageCard.vue.
  const authorHatsByNick = computed(() => messaging.roomHats.value);

  // Per-occupant MUC authority (affiliation + role) sourced live
  // from each inbound MUC presence. Distinct from `authorHatsByNick`:
  // authority is XEP-0045 and server-enforced; hats are XEP-0317
  // descriptive metadata with no protocol semantics. UI surfaces
  // that render OWNER / ADMIN / MOD chips read from here.
  const authorAuthorityByNick = computed(() => messaging.roomAuthority.value);

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

  watch(() => messaging.pendingNotificationActivities.value, (events) => {
    if (events.length === 0) return;

    showForegroundNotificationsForChannelActivities(events, {
      notifySettings,
      notifications,
      messageSound,
      messageSoundsEnabled: () => notifications.messageSoundsEnabled.value,
      canShowForegroundNotification: () => notifications.canShowForegroundNotifications.value,
      isDoNotDisturb: () => selfPresenceShow.value === "dnd",
      isTabFocused: () => isWindowFocused.value,
      sessionJid: connectionStore.session?.jid,
      resolveChannelNameFromJid,
      onNavigate: (roomJid) => {
        const managedRoom = parseManagedRoomBareJid(roomJid);
        if (!managedRoom) return;
        void selectChannel(managedRoom.channelId);
      },
    });

    messaging.pendingNotificationActivities.value = [];
  });

  watch(xmppClient, (client) => {
    if (!client || !session.value) return;
    client.setDirectMessageHandler((msg) => {
      dmMessaging.onIncomingMessage(msg);
      dmConversations.receiveIncomingDm(msg);
      showForegroundNotificationForDmActivity(msg, {
        notifySettings,
        notifications,
        messageSound,
        messageSoundsEnabled: () => notifications.messageSoundsEnabled.value,
        canShowForegroundNotification: () => notifications.canShowForegroundNotifications.value,
        isDoNotDisturb: () => selfPresenceShow.value === "dnd",
        isTabFocused: () => isWindowFocused.value,
        sessionJid: session.value?.jid,
        activePeerJid: ui.sidebarMode.value === "dms" ? dmConversations.activePeerJid.value : null,
        onNavigate: (peerJid) => {
          void handleOpenDm(peerJid);
        },
      });
    });
    client.setDmChatStateHandler(dmMessaging.onChatState);
    client.setDmDisplayedHandler(dmMessaging.onDisplayed);
    client.setDmReactionHandler(dmMessaging.onReaction);
    // XEP-0490 §3.2: another resource of this account has marked a
    // chat as displayed. Persist the stanza-id under the MDS-scoped
    // last-seen key so existing conversation-scoped readers can pick
    // it up alongside their own divider state. The chat-id is the
    // bare JID of either the MUC room or the DM peer.
    client.setMdsDisplayedHandler((entry) => {
      const chatId = barePeerJid(entry.chatId);
      const displayed = {
        stanzaId: entry.stanzaId,
        stanzaIdBy: barePeerJid(entry.stanzaIdBy),
      };
      const accepted = isActiveDirectDmSurface()
        ? dmMessaging.applyMdsDisplayed(chatId, displayed)
        : messaging.applyMdsDisplayed(chatId, displayed);
      const key = mdsChatKey(chatId);
      if (accepted) setMdsDisplayed(key, displayed);
      else queueMdsDisplayed(key, displayed);
    });
    client.setPresenceUpdateHandler((event) => {
      if (barePeerJid(event.bareJid) === barePeerJid(session.value?.jid ?? "")) {
        selfPresenceShow.value = event.show;
      }
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
      // Short-circuit if the session is already torn down — a
      // lifecycle event queued before `handleLogout` ran can fire
      // here AFTER `notifySettings.reset()` and would
      // otherwise restart hydrate against the about-to-disconnect
      // client. Round-12 reviewer P1.
      if (!connectionStore.session) return;
      // Re-hydrate inbox on every XMPP session-ready, both resumed and
      // fresh. Stream resume catches up on stanzas the client missed
      // while disconnected, but a *fresh* reconnection (resume failed
      // — too much time elapsed, server restart, network blip past the
      // resume window) means we lost the push stream entirely and the
      // local unread map is stale. `onConnectionReady` only hydrates
      // on the first sign-in (one-shot `hasBootstrapped` guard), so
      // subsequent fresh reconnections would otherwise never refresh.
      // `hydrateFromInbox` is request-id deduped, so the redundant
      // call on the very first connection is harmless.
      void dmConversations.hydrateFromInbox();
      void channelUnread.hydrateFromInbox();
      void socialFeed.refresh();
      void stories.refresh();
      void communityEvents.refresh();
      // Re-hydrate XEP-0492 notification settings only on *fresh*
      // reconnects. A stream resume is by definition gap-free —
      // any bookmark publish from another tab during the disconnect
      // is impossible because we never disconnected as far as the
      // server's PEP queue is concerned. Refetching on every resume
      // burns one IQ round-trip per resume for no payoff (round-12
      // reviewer P2). Until the chat subscribes to PEP `+notify`
      // headlines on `urn:xmpp:bookmarks:1` (deferred follow-up),
      // fresh-only hydrate is the correct cadence.
      if (event.type === "fresh") {
        // Belt-and-braces: hydrate already catches lower-layer
        // throws, but call-site .catch defends against any future
        // regression so an unhandled rejection doesn't propagate
        // out of the lifecycle handler. Round-14 PR review.
        notifySettings.hydrate(client).catch(() => undefined);
      }
    });
  }, { immediate: true });

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

  function handleToggleMessageSounds() {
    notifications.messageSoundsEnabled.value = !notifications.messageSoundsEnabled.value;
  }

  async function refreshAppUpdate() {
    await appUpdate.applyUpdate();
  }

  const activeUploadProgress = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.uploadProgress.value : messaging.uploadProgress.value,
  );
  const activeActionError = computed(() => ui.actionError.value);
  const activeErrorActionLabel = computed(() => {
    const peer = activeDmPeer.value;
    return isActiveDirectDmSurface() &&
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
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) {
    if (isActiveDirectDmSurface()) {
      await dmMessaging.sendMessage(body, markup, references, files, replyTo, linkPreview);
      return;
    }
    await messaging.sendMessage(body, markup, references, files, replyTo, forumTitle, linkPreview);
  }

  async function sendPublicChannelMessage(body: string) {
    if (ui.sidebarMode.value !== "channels" || !xmppClient.value || !waddles.activeChannelId.value) {
      throw new Error("Public AI prompts require an active channel.");
    }
    ui.clearActionError();
    await messaging.sendMessage(body);
    if (ui.actionError.value) {
      throw new Error(ui.actionError.value);
    }
  }

  async function sendThreadMessage(
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) {
    if (isActiveDirectDmSurface()) {
      await dmMessaging.sendMessage(body, markup, references, files, replyTo, linkPreview, threadOverride);
      return;
    }
    await messaging.sendMessage(body, markup, references, files, replyTo, threadOverride, linkPreview);
  }

  async function sendCallChatMessage(
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) {
    ui.clearActionError();
    await sendThreadMessage(body, markup, references, files, replyTo, threadOverride, linkPreview);
    if (ui.actionError.value) {
      throw new Error(ui.actionError.value);
    }
  }

  function sendGif(
    url: string,
    threadOverride?: { threadId: string; parentThreadId?: string },
  ) {
    if (threadOverride) {
      void sendThreadMessage(url, [], [], undefined, undefined, threadOverride);
      return;
    }
    void sendActiveMessage(url, [], []);
  }

  function openThread(threadId: string, targetMessageId?: string) {
    if (!threadId) return;
    activeRightPanel.value = "thread";
    activeThreadTargetMessageId.value = targetMessageId ?? null;
    if (
      activeThreadStack.value.length > 0 &&
      activeThreadStack.value[activeThreadStack.value.length - 1] === threadId
    ) {
      if (targetMessageId) backfillActiveThread(threadId);
      return;
    }
    activeThreadStack.value = [threadId];
    backfillActiveThread(threadId);
  }

  // Backfill a thread's replies from the archive for the active surface. DMs
  // query the personal archive (`with=peer` + thread); channels query the
  // room archive. Without this a thread opened from the global Threads view
  // or a deep link shows only the replies that happen to be in the loaded
  // conversation window.
  function backfillActiveThread(threadId: string) {
    if (isActiveDirectDmSurface()) {
      void dmMessaging.backfillThread(threadId);
    } else if (ui.sidebarMode.value === "channels") {
      void messaging.backfillThread(threadId);
    } else if (waddles.currentChannel.value?.isGroupDm) {
      void messaging.backfillThread(threadId);
    }
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

  // Global Threads view (`urn:waddle:threads:0`) rows carry a bare JID,
  // which may be a channel (MUC) room or a DM partner. Route each to its
  // own surface — channel selection vs DM open — then open the thread
  // panel. Without this, DM rows fell through `selectChannel(<localpart>)`
  // and tried to open a nonexistent channel (#917).
  async function onSelectThreadEntry(channelJid: string, threadId: string) {
    const target = resolveThreadEntryTarget(channelJid, {
      channels: waddles.channels.value,
      managedMucDomain: managedMucDomain.value,
    });
    if (!target) return;
    if (target.kind === "channel") {
      await onSelectThread(target.channelId, threadId);
      return;
    }
    await handleOpenDm(target.peerJid);
    openThread(threadId);
  }

  function pushThread(threadId: string) {
    if (!threadId) return;
    activeRightPanel.value = "thread";
    activeThreadTargetMessageId.value = null;
    if (
      activeThreadStack.value.length > 0 &&
      activeThreadStack.value[activeThreadStack.value.length - 1] === threadId
    ) {
      return;
    }
    activeThreadStack.value = [...activeThreadStack.value, threadId];
    backfillActiveThread(threadId);
  }

  function popThreadTo(index: number) {
    activeThreadTargetMessageId.value = null;
    if (index < 0) {
      activeThreadStack.value = [];
      normalizeActiveRightPanel("thread");
      return;
    }
    activeThreadStack.value = activeThreadStack.value.slice(0, index + 1);
    activeRightPanel.value = "thread";
  }

  function closeThreadPanel() {
    activeThreadTargetMessageId.value = null;
    activeThreadStack.value = [];
    normalizeActiveRightPanel("thread");
  }

  function notifyActiveComposing(
    threadOverride?: { threadId: string; parentThreadId?: string },
  ) {
    // XEP-0201 §3: when typing originates in a thread composer, the
    // outbound XEP-0085 stanza echoes the active thread so peers can
    // scope the indicator instead of treating it as channel-wide.
    const thread = threadOverride
      ? threadOverride.parentThreadId
        ? { id: threadOverride.threadId, parent: threadOverride.parentThreadId }
        : { id: threadOverride.threadId }
      : undefined;
    activeTarget.value.notifyComposing(thread);
  }

  function editActiveMessage(messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[], linkPreview?: ComposerLinkPreviewSendPayload) {
    if (isActiveDirectDmSurface()) {
      void dmMessaging.editMessage(messageId, newBody, markup, references, linkPreview);
      return;
    }
    void messaging.editMessage(messageId, newBody, markup, references, linkPreview);
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
    if (!client) return;
    const stanzaId = resolvePinTargetStanzaId(messageId);
    if (!stanzaId) return;
    if (isActiveDirectDmSurface()) {
      const peer = dmConversations.activePeerJid.value;
      if (!peer || !("pinDirectMessage" in client)) return;
      void client.pinDirectMessage(peer, stanzaId).catch((error: unknown) => {
        console.warn("pinDirectMessage failed", error);
      });
      return;
    }
    const channel = waddles.currentChannel.value;
    if (!channel) return;
    void client.pinMessage(channel.spaceId ?? "", channel.id, stanzaId).catch((error: unknown) => {
      console.warn("pinMessage failed", error);
    });
  }

  function unpinActiveMessage(messageId: string) {
    const client = xmppClient.value;
    if (!client) return;
    const stanzaId = resolvePinTargetStanzaId(messageId);
    if (!stanzaId) return;
    if (isActiveDirectDmSurface()) {
      const peer = dmConversations.activePeerJid.value;
      if (!peer || !("unpinDirectMessage" in client)) return;
      void client.unpinDirectMessage(peer, stanzaId).catch((error: unknown) => {
        console.warn("unpinDirectMessage failed", error);
      });
      return;
    }
    const channel = waddles.currentChannel.value;
    if (!channel) return;
    void client.unpinMessage(channel.spaceId ?? "", channel.id, stanzaId).catch((error: unknown) => {
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
    const message = activeTarget.value.messages.value.find((m) => m.id === messageId);
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
    if (!isActiveDirectDmSurface() || !peer) return;
    void dmMessaging.loadMessages(peer.peerJid);
  }

  function ensureActiveMessageLoaded(messageId: string) {
    return activeTarget.value.ensureMessageLoaded(messageId);
  }

  function loadOlderThreadMessages(threadId: string) {
    if (isActiveDirectDmSurface()) {
      void dmMessaging.loadOlderThreadMessages(threadId);
      return;
    }
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
    // Don't intercept Escape when any dialog/drawer is open so they can close first.
    if (anyModalOpen()) return;
    if (activeRightPanel.value === "extension" && activeExtensionRouteKey.value) {
      closeExtensionRoutePanel();
      consumeKeystrokEvent(event);
      return;
    }
    if (activeRightPanel.value === "pinned" && ui.showPinnedPanel.value) {
      closePinnedPanel();
      consumeKeystrokEvent(event);
      return;
    }
    if (activeThreadStack.value.length === 0) return;
    activeThreadStack.value = activeThreadStack.value.slice(0, -1);
    normalizeActiveRightPanel();
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
    // Community-surface routes win over the page-switch ladder: a
    // watcher firing while activeCommunitySurface is set must not
    // bounce the URL back to channel/home before the surface clears.
    const surface = ui.activeCommunitySurface.value;
    if (surface === "feed" || surface === "events") {
      navigate({ id: surface });
      return;
    }
    if (ui.activePage.value === "settings") {
      navigate({ id: "settings", origin: "app" });
      return;
    }
    if (ui.activePage.value === "threads") {
      // Without this branch the fall-through below would push the
      // home route ("/") every time a watcher fires after
      // `openThreads()` — bouncing the URL off /threads immediately,
      // and reverting /threads back to / on initial load when
      // `onConnectionReady`'s `finally` calls updateUrl.
      navigate({ id: "threads" });
      return;
    }
    if (ui.activePage.value === "unread") {
      // Mirror the threads branch: keep the URL pinned to /unread while
      // watchers fire, instead of falling through to the home route.
      navigate({ id: "unread" });
      return;
    }
    if (ui.activePage.value === "admin") {
      // Admin owns the entire workspace and is mounted out of
      // ChatReadyShell's own popstate-driven adminPanelRef, but
      // updateUrl still needs an admin-aware branch so a stray
      // activePage watcher doesn't drag the user back to /.
      return;
    }
    if (ui.activePage.value === "dashboard") {
      navigate({ id: "home" });
      return;
    }
    const channel = waddles.currentChannel.value;
    if (ui.activePage.value === "chat" && activeRightPanel.value === "extension") {
      const ext = activeExtensionRouteKey.value;
      if (channel && ext) {
        navigate({
          id: "channelExtension",
          params: { channelId: channel.id, pluginId: ext.pluginId, routeId: ext.routeId },
          search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
        });
      } else {
        navigate({ id: "home" });
      }
      return;
    }
    if (ui.sidebarMode.value === "dms") {
      if (activeDmPeer.value) {
        navigate({
          id: "dm",
          params: { username: activeDmPeer.value.peerUsername },
          search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
        });
      } else if (channel?.isGroupDm && channel.jid) {
        navigate({
          id: "groupDmRoom",
          params: { roomJid: channel.jid },
          search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
        });
      } else {
        navigate({ id: "dmList" });
      }
    } else if (channel) {
      navigate({
        id: "channel",
        params: { channelId: channel.id },
        search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
      });
    } else {
      navigate({ id: "home" });
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
      activeExtensionRouteKey.value = null;
      activeRightPanel.value = null;
      exitReactionMode();
      updateUrl();
    },
  );

  watch(activeThreadStack, () => {
    normalizeActiveRightPanel();
    exitReactionMode();
    updateUrl();
  }, { deep: true });
  // #414: any toggle of the pin panel pushes the URL state.
  watch(() => ui.showPinnedPanel.value, () => {
    if (!ui.showPinnedPanel.value && activeRightPanel.value === "pinned") {
      normalizeActiveRightPanel("pinned");
    }
    updateUrl();
  });
  // Waddle MAM stanza-id filter: on false → true transition, batch-fetch
  // any pinned stanza-ids not already in the loaded timeline or cache.
  watch(() => ui.showPinnedPanel.value, async (open) => {
    if (!open) return;
    if (isActiveDirectDmSurface()) return;
    const client = xmppClient.value;
    const spaceId = waddles.currentChannel.value?.spaceId ?? "";
    const channelId = waddles.activeChannelId.value;
    const roomJid = messaging.currentRoomJid.value;
    if (!client || !channelId || !roomJid) return;
    if (!("fetchRoomMessagesByStanzaIds" in client)) return;
    const convertForTimeline = (a: Parameters<typeof roomMessageFromArchived>[0]) => {
      const live = roomMessageFromArchived(a, {
        trustedMediaOrigin: session.value
          ? trustedLinkPreviewMediaOrigin(session.value)
          : null,
      });
      return live && session.value
        ? mapLiveRoomMessageToTimeline(session.value, live)
        : null;
    };
    try {
      await hydratePinnedBodiesOnPanelOpen({
        fetchByStanzaIds: (stanzaIds) =>
          client.fetchRoomMessagesByStanzaIds(spaceId, channelId, stanzaIds),
        spaceId,
        channelId,
        roomJid,
        timelineMessages: messaging.messages.value,
        convert: convertForTimeline,
      });
    } catch (error) {
      console.warn("hydratePinnedBodiesOnPanelOpen failed", error);
    }
  });
  watch(() => ui.showPinnedPanel.value, async (open) => {
    if (!open || !isActiveDirectDmSurface()) return;
    const client = xmppClient.value;
    const peerJid = dmConversations.activePeerJid.value;
    const currentSession = session.value;
    if (!client || !peerJid || !currentSession) return;
    if (!("fetchDirectMessagesByStanzaIds" in client)) return;
    const convertForTimeline = (archived: Parameters<typeof dmMessageFromArchived>[0]) => {
      const live = dmMessageFromArchived(archived, barePeerJid(currentSession.jid), {
        trustedMediaOrigin: trustedLinkPreviewMediaOrigin(currentSession),
      });
      return live ? fromLiveDmMessage(currentSession, live) : null;
    };
    try {
      await hydratePinnedBodiesOnPanelOpen({
        fetchByStanzaIds: (stanzaIds) =>
          client.fetchDirectMessagesByStanzaIds(peerJid, stanzaIds),
        spaceId: "",
        channelId: "",
        roomJid: peerJid,
        timelineMessages: dmMessaging.messages.value,
        convert: convertForTimeline,
      });
    } catch (error) {
      console.warn("hydratePinnedBodiesOnPanelOpen failed", error);
    }
  });
  watch(pinnedRooms, (rooms) => {
    if (!ui.showPinnedPanel.value || !isActiveDirectDmSurface()) return;
    const client = xmppClient.value;
    const peerJid = dmConversations.activePeerJid.value;
    const currentSession = session.value;
    if (!client || !peerJid || !currentSession) return;
    if (!("fetchDirectMessagesByStanzaIds" in client)) return;
    const state = rooms.get(peerJid);
    const entry = state?.entries[0];
    if (!entry) return;
    const convertForTimeline = (archived: Parameters<typeof dmMessageFromArchived>[0]) => {
      const live = dmMessageFromArchived(archived, barePeerJid(currentSession.jid), {
        trustedMediaOrigin: trustedLinkPreviewMediaOrigin(currentSession),
      });
      return live ? fromLiveDmMessage(currentSession, live) : null;
    };
    void hydrateSinglePinnedBody({
      fetchByStanzaIds: (stanzaIds) =>
        client.fetchDirectMessagesByStanzaIds(peerJid, stanzaIds),
      spaceId: "",
      channelId: "",
      roomJid: peerJid,
      stanzaId: entry.target_stanza_id,
      timelineMessages: dmMessaging.messages.value,
      convert: convertForTimeline,
    }).catch((error) => console.warn("hydrateSinglePinnedBody failed", error));
  });
  watch(
    [xmppClient, () => ui.sidebarMode.value, () => dmConversations.activePeerJid.value],
    ([client, mode, peerJid]) => {
      if (mode !== "dms" || !peerJid) return;
      if (!client || !("fetchDirectPins" in client)) {
        hydratePinnedRoom(peerJid, []);
        return;
      }
      const epoch = pinnedRoomsEpoch();
      void client.fetchDirectPins(peerJid)
        .then((entries) => {
          if (!isActiveDirectDmSurface() || dmConversations.activePeerJid.value !== peerJid) return;
          hydratePinnedRoom(peerJid, entries, epoch);
        })
        .catch((error: unknown) => {
          console.warn("fetchDirectPins failed", error);
          hydratePinnedRoom(peerJid, [], epoch);
        });
    },
    { immediate: true },
  );
  watch(activeExtensionRouteKey, () => {
    normalizeActiveRightPanel();
  }, { deep: true });
  watch(activeRightPanel, () => {
    updateUrl();
  });
  watch(() => ui.activePage.value, () => {
    normalizeActiveRightPanel();
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
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "settings";
  }

  /**
   * Navigate to the global Threads view. Clears any channel/DM
   * selection, drops local thread-panel state, syncs the URL so a
   * page refresh lands back on /threads.
   */
  function openThreads() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "threads";
    // ChatReadyShell renders FeedPane / EventsPane on
    // `activeCommunitySurface` v-else-if branches BEFORE the threads
    // branch — leaving the surface non-null would let one of those win
    // and ThreadsView would silently not render even though the URL
    // changed. Clear the surface so the threads v-else-if matches.
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "threads" });
  }

  /**
   * Navigate to the global Unread view. Mirrors `openThreads()`: clears
   * any channel/DM selection and local thread-panel state, clears the
   * active community surface so the `unread` v-else-if branch in
   * ChatReadyShell wins, and syncs the URL so a refresh lands on /unread.
   */
  function openUnread() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "unread";
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "unread" });
  }

  /**
   * Navigate to a community surface — Feed / Events. These
   * are first-class routes (`/feed`, `/events`) but they
   * also need the in-page state set immediately so the v-else-if
   * branches in ChatReadyShell re-render this tick. `pushState` (via
   * `navigate()`) doesn't fire `popstate`, so the controller's
   * popstate-driven state sync only covers back/forward; in-app
   * clicks need to mirror what `applyRouteTarget` would do.
   */
  function openCommunitySurface(surface: "feed" | "events") {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "chat";
    ui.activeCommunitySurface.value = surface;
    if (surface === "feed") {
      ui.feedDefaultFilter.value = "all";
      ui.feedDefaultComposerMode.value = "post";
    }
    ui.sidebarMode.value = "channels";
    dmConversations.closeDm();
    waddles.activeChannelId.value = null;
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: surface });
  }

  /**
   * Navigate back to the Home dashboard from anywhere. Clears any
   * channel/DM selection, drops thread state, closes the mobile nav
   * drawer, and resyncs the URL so a refresh returns the user to home
   * rather than re-entering the last channel.
   */
  function openHome() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "dashboard";
    ui.sidebarMode.value = "channels";
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "home" });
  }

  /**
   * Navigate to the DM list (`/dm`). Mirrors `openHome` / `openThreads`:
   * mutates the in-page state up front so the v-else-if cascade in
   * ChatReadyShell switches to DM mode this tick, then pushes the URL
   * so refresh and back/forward land on the same place.
   */
  function openDmList() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "dms";
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "dmList" });
  }

  function closeUserSettings() {
    const state = window.history.state as { waddleRouteId?: string; origin?: string } | null;
    if (
      window.location.pathname === "/settings"
      && state?.waddleRouteId === "settings"
      && state.origin === "app"
    ) {
      window.history.back();
      return;
    }
    // Set the page we want to land on; updateUrl() reads the rest of
    // the controller state and constructs the right typed match
    // (channel / extension / DM / home / community surface).
    ui.activePage.value = waddles.currentChannel.value || activeDmPeer.value
      ? "chat"
      : "dashboard";
    updateUrl();
  }

  // Single mapping from the typed route match to the page-level shell
  // state (activePage, activeCommunitySurface, sidebarMode,
  // showPinnedPanel). Used by the SSR seed at controller construction
  // and at the top of `applyRouteTarget` — keeping the derivation in
  // one place avoids the seed and the popstate path drifting apart.
  function applyMatchToShellState(match: RouteMatch): void {
    switch (match.id) {
      case "home":
        ui.activePage.value = "dashboard";
        break;
      case "channel":
      case "channelExtension":
      case "dm":
      case "groupDmRoom":
      case "dmList":
      case "feed":
      case "stories":
      case "events":
        ui.activePage.value = "chat";
        break;
      case "settings":
        ui.activePage.value = "settings";
        break;
      case "admin":
        ui.activePage.value = "admin";
        break;
      case "threads":
        ui.activePage.value = "threads";
        break;
      case "unread":
        ui.activePage.value = "unread";
        break;
    }
    ui.activeCommunitySurface.value =
      match.id === "feed" || match.id === "stories" || match.id === "events"
        ? match.id === "stories" ? "feed" : match.id
        : null;
    if (match.id === "stories") {
      ui.feedDefaultFilter.value = "stories";
      ui.feedDefaultComposerMode.value = "story";
    } else if (match.id === "feed") {
      ui.feedDefaultFilter.value = "all";
      ui.feedDefaultComposerMode.value = "post";
    }
    ui.sidebarMode.value =
      match.id === "dm" || match.id === "groupDmRoom" || match.id === "dmList" ? "dms" : "channels";
    // #414/#951: channel and DM conversation routes carry the pinned-panel flag.
    ui.showPinnedPanel.value =
      match.id === "channel" || match.id === "channelExtension" || match.id === "dm" || match.id === "groupDmRoom"
        ? match.search.pinned
        : false;
  }

  function onPopState() {
    // applyRouteTarget is the single state-from-match handler — it
    // covers every route id and is the only place that mutates
    // controller state in response to a URL change. onPopState's job
    // is just to invoke it with the new URL and manage the
    // `isApplyingRoute` lifecycle.
    const match = matchLocation(window.location.pathname, window.location.search);
    pendingChannelRouteMatch = null;
    const requestId = ++routeRequestId;
    isApplyingRoute.value = true;
    void applyRouteTarget(match, requestId).finally(() => {
      if (requestId === routeRequestId) {
        isApplyingRoute.value = false;
      }
    });
  }

  async function applyRouteTarget(match: RouteMatch, requestId: number) {
    clearPendingChannelRoomJidSelection();
    applyMatchToShellState(match);
    if (match.id === "stories") {
      navigate({ id: "feed" }, { replace: true });
    }
    // Routes that don't reference a specific channel/DM target also
    // drop the chat-context state. (Channel/extension/dm routes set
    // their own context further down.)
    if (
      match.id === "home"
      || match.id === "feed"
      || match.id === "stories"
      || match.id === "events"
      || match.id === "dmList"
    ) {
      dmConversations.closeDm();
      waddles.activeChannelId.value = null;
      activeExtensionRouteKey.value = null;
      activeRightPanel.value = null;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      if (match.id === "home") messaging.clearMessages();
      return;
    }
    if (
      match.id === "admin"
      || match.id === "settings"
      || match.id === "threads"
      || match.id === "unread"
    ) {
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      activeExtensionRouteKey.value = null;
      return;
    }
    if (match.id === "dm") {
      activeExtensionRouteKey.value = null;
      const username = match.params.username.replace(/^@/, "").trim();
      if (username) {
        const domain = session.value ? jidDomain(session.value.jid) : "";
        if (!domain) return;
        await handleOpenDm(`${username}@${domain}`);
        if (requestId !== routeRequestId) return;
      }
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      ui.showPinnedPanel.value = match.search.pinned;
      if (match.search.pinned) {
        activeRightPanel.value = "pinned";
      } else if (match.search.thread.length > 0) {
        activeRightPanel.value = "thread";
      } else {
        activeRightPanel.value = null;
      }
      // Dedup: a nested stack can legitimately repeat a thread id (e.g.
      // `[A,B,A]`), but each thread only needs one backfill.
      for (const threadId of new Set(match.search.thread)) {
        void dmMessaging.backfillThread(threadId);
      }
      return;
    }

    if (match.id === "groupDmRoom") {
      activeExtensionRouteKey.value = null;
      dmConversations.closeDm();
      await selectGroupDm(match.params.roomJid, { updateUrl: false });
      if (requestId !== routeRequestId) return;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      ui.showPinnedPanel.value = match.search.pinned;
      activeRightPanel.value = match.search.pinned
        ? "pinned"
        : match.search.thread.length > 0
          ? "thread"
          : null;
      for (const threadId of new Set(match.search.thread)) {
        void messaging.backfillThread(threadId);
      }
      return;
    }


    if (match.id === "channelExtension") {
      ui.sidebarMode.value = "channels";
      dmConversations.closeDm();
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      const ch = resolveChannelBySlug(match.params.channelId, waddles.channels.value);
      if (!ch) {
        waddles.activeChannelId.value = null;
        activeExtensionRouteKey.value = null;
        return;
      }
      waddles.activeChannelId.value = ch.id;
      void waddles.reloadChannelMembers(ch.id);
      const nextExtensionRouteKey = {
        channelId: ch.id,
        pluginId: match.params.pluginId,
        routeId: match.params.routeId,
      };
      messaging.clearMessages();
      await messaging.loadMessages(ch.spaceId ?? "", ch.id);
      if (requestId !== routeRequestId) return;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      ui.showPinnedPanel.value = match.search.pinned;
      for (const threadId of match.search.thread) {
        void messaging.backfillThread(threadId);
      }
      activeExtensionRouteKey.value = nextExtensionRouteKey;
      activeRightPanel.value = "extension";
      return;
    }

    // match.id === "channel"
    activeExtensionRouteKey.value = null;
    const ch = resolveChannelBySlug(match.params.channelId, waddles.channels.value);
    if (!ch) {
      waddles.activeChannelId.value = null;
      messaging.clearMessages();
      return;
    }
    if (ch.isGroupDm && ch.jid) {
      navigate({
        id: "groupDmRoom",
        params: { roomJid: ch.jid },
        search: match.search,
      }, { replace: true });
      await selectGroupDm(ch.jid, { updateUrl: false });
      ui.showPinnedPanel.value = match.search.pinned;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      activeRightPanel.value = match.search.pinned
        ? "pinned"
        : match.search.thread.length > 0
          ? "thread"
          : null;
      for (const threadId of match.search.thread) {
        void messaging.backfillThread(threadId);
      }
      return;
    }
    waddles.activeChannelId.value = ch.id;
    void waddles.reloadChannelMembers(ch.id);
    messaging.clearMessages();
    await messaging.loadMessages(ch.spaceId ?? "", ch.id);

    // Restore the thread panel from the URL and initialize paging for every
    // visible thread pane. Dedupe in the messaging composable keeps already
    // loaded roots/replies stable.
    ui.showPinnedPanel.value = match.search.pinned;
    activeThreadTargetMessageId.value = null;
    activeThreadStack.value = match.search.thread;
    if (match.search.pinned) {
      activeRightPanel.value = "pinned";
    } else if (match.search.thread.length > 0) {
      activeRightPanel.value = "thread";
    } else {
      activeRightPanel.value = null;
    }
    for (const threadId of match.search.thread) {
      void messaging.backfillThread(threadId);
    }
  }

  // --- Bootstrap (watches connection store) ---

  let initialStructureLoadFinished = false;
  let missingStructureOnlineEpoch = messaging.xmppStatus.value.state === "online" ? 1 : 0;
  let lastMissingStructureRefreshEpoch = 0;
  let missingStructureRefreshPromise: Promise<void> | null = null;
  let pendingChannelRouteMatch: RouteMatch | null = null;

  function routeNeedsDiscoveredChannel(match: RouteMatch): boolean {
    return match.id === "channel" || match.id === "channelExtension" || match.id === "groupDmRoom";
  }

  function channelRouteTargetMissing(match: RouteMatch): boolean {
    if (match.id === "groupDmRoom") {
      const roomJid = barePeerJid(match.params.roomJid);
      return !waddles.groupDms.value.some((group) => barePeerJid(group.roomJid) === roomJid);
    }
    if (match.id !== "channel" && match.id !== "channelExtension") return false;
    return resolveChannelBySlug(match.params.channelId, waddles.channels.value) == null;
  }

  async function applyPendingChannelRouteAfterStructure() {
    if (!pendingChannelRouteMatch || waddles.channels.value.length === 0) return;
    const match = matchLocation(window.location.pathname, window.location.search);
    if (!routeNeedsDiscoveredChannel(match)) {
      pendingChannelRouteMatch = null;
      return;
    }
    if (channelRouteTargetMissing(match)) return;
    pendingChannelRouteMatch = null;
    const requestId = ++routeRequestId;
    isApplyingRoute.value = true;
    try {
      await refreshExtensionRoutes();
      if (requestId === routeRequestId) {
        await applyRouteTarget(match, requestId);
      }
    } finally {
      if (requestId === routeRequestId) {
        isApplyingRoute.value = false;
        updateUrl();
      }
    }
  }

  async function refreshMissingStructureAfterReconnect() {
    const retryEpoch = missingStructureOnlineEpoch;
    const currentMatch = matchLocation(window.location.pathname, window.location.search);
    const routeTargetMissing = channelRouteTargetMissing(currentMatch);
    pendingChannelRouteMatch = routeTargetMissing ? currentMatch : null;
    if (!shouldRetryMissingStructureLoad({
      appReady: connectionStore.appState === "ready",
      hasClient: xmppClient.value !== null,
      initialLoadFinished: initialStructureLoadFinished,
      inFlight: missingStructureRefreshPromise !== null,
      isLoadingStructure: waddles.isLoadingStructure.value,
      spaceCount: waddles.waddles.value.length,
      channelCount: waddles.channels.value.length,
      routeTargetMissing,
      xmppStatus: messaging.xmppStatus.value.state,
      onlineEpoch: retryEpoch,
      lastAttemptedOnlineEpoch: lastMissingStructureRefreshEpoch,
    })) {
      return;
    }

    lastMissingStructureRefreshEpoch = retryEpoch;
    const activeChannelId = waddles.activeChannelId.value;
    const preserveActiveChannel = shouldPreserveActiveChannelDuringStructureRetry({
      activeChannelListed: activeChannelId !== null && waddles.channels.value.some((channel) => channel.id === activeChannelId),
      routeTargetMissing,
    });
    const promise = (
      preserveActiveChannel
        ? waddles.loadStructure(activeChannelId)
        : waddles.loadStructure(null, { noChannelSelect: true })
    ).then(() => undefined);
    missingStructureRefreshPromise = promise;
    try {
      await promise;
      await applyPendingChannelRouteAfterStructure();
    } finally {
      if (missingStructureRefreshPromise === promise) {
        missingStructureRefreshPromise = null;
      }
    }
  }

  async function onConnectionReady() {
    const match = matchLocation(window.location.pathname, window.location.search);
    const requestId = ++routeRequestId;
    isApplyingRoute.value = true;
    let preserveCurrentUrl = false;

    try {
      // Always pass noChannelSelect — channel-targeting routes
      // (`channel` / `channelExtension`) get their active channel set
      // by applyRouteTarget from match.params.channelId, and every
      // other route doesn't want a channel active at all. Auto-select
      // would briefly highlight an arbitrary channel before
      // applyRouteTarget cleared it (visible flicker on /events, /feed,
      // /stories, /threads, …).
      try {
        if (waddles.channels.value.length === 0) {
          await waddles.loadStructure(null, { noChannelSelect: true });
        }
      } finally {
        initialStructureLoadFinished = true;
      }
      if (channelRouteTargetMissing(match)) {
        pendingChannelRouteMatch = match;
      }
      await refreshMissingStructureAfterReconnect();
      if (channelRouteTargetMissing(match)) {
        pendingChannelRouteMatch = match;
        preserveCurrentUrl = true;
      } else {
        await refreshExtensionRoutes();
      }
      if (!preserveCurrentUrl && requestId === routeRequestId) {
        await applyRouteTarget(match, requestId);
      }
      showFirstRunSetupIfNeeded();
    } finally {
      if (requestId === routeRequestId) {
        isApplyingRoute.value = false;
        if (!preserveCurrentUrl) {
          updateUrl();
        }
      }
    }

    void dmConversations.hydrateFromInbox();
    void channelUnread.hydrateFromInbox();
    void rosterContacts.loadRosterContacts();
    void socialFeed.refresh();

    // Hydrate XEP-0492 per-chat notification settings from the user's
    // XEP-0402 PEP bookmarks. Best-effort — an empty result is the
    // first-run state and the UI falls back to the §3 conversation
    // default via [[effectiveNotifyMode]].
    void (async () => {
      const client = xmppClient.value;
      if (!client) return;
      // Best-effort: hydrate already swallows lower-layer
      // exceptions, but a defensive .catch keeps the IIFE quiet
      // even if a future regression bypasses the inner guard.
      // Round-14 PR review.
      await notifySettings.hydrate(client).catch(() => undefined);
    })();

    // Register service worker and sync push subscription (best-effort, non-blocking)
    void (async () => {
      await notifications.registerServiceWorker();
      await setupPushSubscription();
    })();
  }

  // --- Actions ---

  async function handleLogout() {
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "dashboard";
    messaging.disconnect();
    dmMessaging.disconnect();
    waddles.clearData();
    channelUnread.clearAll();
    rosterContacts.clearRosterContacts();
    extensionRoutes.value = [];
    selectedChannelRoomJids.value = {};
    activeExtensionRouteKey.value = null;
    activeRightPanel.value = null;
    setupPromptShown = false;
    messaging.clearMessages();
    dmMessaging.clearMessages();
    // #414: drop all pin state on logout so a subsequent login doesn't
    // see the prior user's pinned-message previews and pre-hydration
    // events buffered from the prior session don't leak forward.
    resetPinnedRooms();
    // #532: drop the XEP-0492 settings cache so a subsequent
    // sign-in does not leak the previous account's per-chat modes
    // into UI reads while the fresh `hydrate` is still in flight.
    notifySettings.reset();
    ui.showPinnedPanel.value = false;
    navigate({ id: "home" });
    await connectionStore.logout();
  }

  async function selectChannel(channelId: string, options: { roomJid?: string; surface?: "channels" | "dms" } = {}) {
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "chat";
    ui.sidebarMode.value = options.surface ?? "channels";
    activeExtensionRouteKey.value = null;
    if (ui.sidebarMode.value !== "dms") dmConversations.closeDm();
    memberJidByNick.value = {};
    const selectedRoomJid = options.roomJid ? barePeerJid(options.roomJid) : null;
    if (selectedRoomJid) {
      selectedChannelRoomJids.value = {
        ...selectedChannelRoomJids.value,
        [channelId]: selectedRoomJid,
      };
      xmppClient.value?.rememberRoomJidForChannel(channelId, selectedRoomJid);
      messaging.rememberChannelRoomJid(channelId, selectedRoomJid);
    }
    waddles.activeChannelId.value = channelId;
    void waddles.reloadChannelMembers(channelId);
    messaging.clearMessages();
    // XEP-0502: Clear activity indicator for this channel
    const roomJid = selectedRoomJid ?? roomJidForChannelId(channelId);
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

  async function selectChannelByRoomJid(roomJid: string) {
    const normalizedRoomJid = barePeerJid(roomJid);
    if (!normalizedRoomJid) return;
    const channelId = knownChannelIdForRoomJid(
      normalizedRoomJid,
      waddles.channels.value,
      managedMucDomain.value,
    );
    if (!channelId) {
      if (isTrustedManagedRoomJid(normalizedRoomJid, managedMucDomain.value)) {
        pendingChannelRoomJidSelection.value = normalizedRoomJid;
        ui.activePage.value = "chat";
        ui.sidebarMode.value = "channels";
        ui.activeCommunitySurface.value = null;
        activeExtensionRouteKey.value = null;
        dmConversations.closeDm();
        memberJidByNick.value = {};
        ui.showMobileNav.value = false;
      }
      return;
    }
    pendingChannelRoomJidSelection.value = null;
    await selectChannel(channelId, { roomJid: normalizedRoomJid });
  }

  async function selectGroupDm(roomJid: string, options: { updateUrl?: boolean } = {}) {
    const normalizedRoomJid = barePeerJid(roomJid);
    const group = waddles.groupDms.value.find((candidate) => barePeerJid(candidate.roomJid) === normalizedRoomJid);
    const channelId = group?.id ?? knownChannelIdForRoomJid(
      normalizedRoomJid,
      waddles.groupDms.value.map((groupDm) => ({
        id: groupDm.id,
        name: groupDm.name,
        jid: groupDm.roomJid,
        isGroupDm: true,
      })),
      managedMucDomain.value,
    );
    if (!channelId) {
      ui.actionError.value = "Group message is not available yet.";
      navigate({ id: "dmList" }, { replace: true });
      return false;
    }
    dmConversations.closeDm();
    await selectChannel(channelId, { roomJid: normalizedRoomJid, surface: "dms" });
    if (options.updateUrl !== false) updateUrl();
    return true;
  }

  async function selectExtensionRoute(channelId: string, route: DiscoveredExtensionRoute) {
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "channels";
    dmConversations.closeDm();
    memberJidByNick.value = {};
    if (waddles.activeChannelId.value !== channelId) {
      waddles.activeChannelId.value = channelId;
      messaging.clearMessages();
      await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", channelId);
    }
    activeExtensionRouteKey.value = { channelId, pluginId: route.pluginId, routeId: route.routeId };
    activeRightPanel.value = "extension";
    updateUrl();
    void waddles.reloadChannelMembers(channelId);
    ui.showMobileNav.value = false;
  }

  async function handleOpenDm(peerJid: string) {
    clearPendingChannelRoomJidSelection();
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

  async function handleCreateGroupDm(payload: { name: string; memberJids: string[] }) {
    const client = xmppClient.value;
    if (!client) {
      ui.actionError.value = "XMPP session is not ready.";
      return;
    }
    if (payload.memberJids.length < 2) {
      ui.actionError.value = "Choose at least two contacts.";
      return;
    }
    waddles.isSubmitting.value = true;
    ui.clearActionError();
    try {
      const created = await client.createGroupDm(payload.name, payload.memberJids);
      await waddles.loadStructure(null, { noChannelSelect: true });
      ui.showNewGroupDm.value = false;
      await selectGroupDm(created.roomJid);
    } catch (error) {
      ui.actionError.value = ui.normalizeError(error);
    } finally {
      waddles.isSubmitting.value = false;
    }
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
    const ok = await waddles.updateChannel();
    if (ok) ui.showEditChannel.value = false;
  }

  async function handleDeleteChannel() {
    ui.confirmDeleteChannel.value = true;
  }

  async function handleMoveChannelToSpace(targetSpaceId: string) {
    const channel = waddles.currentChannel.value;
    if (!channel) return;
    const ok = await waddles.moveChannelToSpace(channel.id, targetSpaceId);
    if (ok) ui.showEditChannel.value = false;
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

  watch(
    () => messaging.xmppStatus.value.state,
    (state, previousState) => {
      if (state === "online" && previousState !== "online") {
        missingStructureOnlineEpoch += 1;
        void refreshMissingStructureAfterReconnect();
      }
    },
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
      socialFeed,
      stories,
      communityEvents,
      dmMessaging,
      xmppClient,
      notifySettings,
      activeMessages,
      activeFirstUnseenId,
      extensionRoutes,
      channelExtensionRoutes,
      activeExtensionRouteKey,
      activeExtensionRoute,
      activeChannelRoomJid,
      activeThreadStack,
      activeThreadTargetMessageId,
      activeRightPanel,
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
      groupDmConversations,
      totalTabUnreadCount,
      notifications,
      appUpdate,
      version,
      avatarUrlByAuthor,
      membersWithAvatars,
      inferredMemberJids,
      authorHatsByNick,
      authorAuthorityByNick,
      activeActionError,
      activeErrorActionLabel,
      activeUploadProgress,
      setContentAreaRef,
      getThreadLabel,
      refreshAppUpdate,
      handleRequestNotifications,
      handleToggleNotifications,
      handleToggleMessageSounds,
      openUserSettings,
      openHome,
      openDmList,
      openThreads,
      openUnread,
      openCommunitySurface,
      closeUserSettings,
      handleLogout,
      selectChannel,
      selectChannelByRoomJid,
      selectGroupDm,
      onSelectThread,
      onSelectThreadEntry,
      selectExtensionRoute,
      handleOpenDm,
      selectDm,
      handleNewDm,
      handleCreateGroupDm,
      openCreateChannelDialog,
      handleCreateChannel,
      handleUpdateChannel,
      handleDeleteChannel,
      handleMoveChannelToSpace,
      confirmDeleteChannel,
      handleUpdateWaddle,
      handleDeleteWaddle,
      confirmDeleteWaddle,
      handleRemoveMember,
      confirmRemoveMember,
      openChannelEdit,
      sendActiveMessage,
      sendPublicChannelMessage,
      sendThreadMessage,
      sendCallChatMessage,
      sendGif,
      openThread,
      pushThread,
      popThreadTo,
      closeThreadPanel,
      activateRightPanel,
      closeExtensionRoutePanel,
      closePinnedPanel,
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
