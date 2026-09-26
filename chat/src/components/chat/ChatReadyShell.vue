<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $mucCallMedia,
  $mucCallParticipants,
  mucCallParticipantCounts,
} from "@/lib/calls/muc-call-presence";
import { $dmCallActivities } from "@/lib/calls/dm-call-activity";
import {
  activeChannelRailCallCount,
  activeDmRailCallCount,
} from "@/lib/calls/call-rail-counts";
import {
  answerIncomingDmCallActivity,
  endRecoveredDmCallAction,
  resumeDmCallActivity,
} from "@/lib/calls/dm-call-actions";
import { leaveRetainedMucCallAction, startMucCallAction } from "@/lib/calls/muc-call-actions";
import {
  $mucCallTerminatePendingSessions,
  hydrateMucCallTerminatePendingSessions,
} from "@/lib/calls/muc-call-session-cache";
import { normalizeMucServiceDomain } from "@/lib/calls/muc-call-indicators";
import {
  $callState,
  configureIncomingCallAlerts,
  type RawIqSender,
} from "@/lib/calls/call-store";
import type { CallWireSender } from "@/lib/calls/outbound";
import type { CallMedia } from "@/lib/calls/types";
import HomeDashboard from "@/components/chat/HomeDashboard.vue";
import ThreadsView from "@/components/chat/ThreadsView.vue";
import UnreadView from "@/components/chat/UnreadView.vue";
import FeedPane from "@/components/community/FeedPane.vue";
import EventsPane from "@/components/community/EventsPane.vue";
import ChatAppModals from "@/components/chat/ChatAppModals.vue";
import ChatMobileDrawers from "@/components/chat/ChatMobileDrawers.vue";
import ContentArea from "@/components/chat/ContentArea.vue";
import SupersededRecoveryBanner from "@/components/chat/SupersededRecoveryBanner.vue";
import ExtensionRouteRail from "@/components/chat/ExtensionRouteRail.vue";
import ExtensionRouteView from "@/components/chat/ExtensionRouteView.vue";
import ThreadPanel from "@/components/chat/ThreadPanel.vue";
import PinnedPanel from "@/components/chat/PinnedPanel.vue";
import UserSettingsPage from "@/components/chat/UserSettingsPage.vue";
import CommunityShell from "@/components/community/CommunityShell.vue";
import { toast } from "@/ui/toaster";
import RoomsPage from "@/components/community/pages/RoomsPage.vue";
import MembersPage from "@/components/community/pages/MembersPage.vue";
import MemberProfileCard from "@/components/community/pages/MemberProfileCard.vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import AdminView from "@/components/admin/AdminView.vue";
import CallActivityDock from "@/components/calls/CallActivityDock.vue";
import CallAudioPlaybackPrompt from "@/components/calls/CallAudioPlaybackPrompt.vue";
import CurrentCallPanel from "@/components/calls/CurrentCallPanel.vue";
import { navigate, useRouteMatch, type AdminMatch, type AdminPanel } from "@/router";
import { buildHomeDashboardProps } from "@/home/dashboard-props";
import type { MessageThreadEntry } from "@/channels/threads";
import { barePeerJid, jidDomain, jidLocalpart } from "@/lib/xmpp/jid";
import type { ChatAppController } from "@/shell/chat-app-controller";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import { isEventUpcomingOrOngoing, sortEventsUpcomingFirst, type CommunityEvent, type FeedPostInput, type StoryPostInput } from "@/lib/xmpp-client";
import { eventBounds } from "@/lib/xmpp/event-calendar";
import { presenceFromShow, type MemberCardModel } from "@/shell/controllers/use-people-rail";
import type { ActivityPublication, MoodPublication, TunePublication } from "@/lib/xmpp/pep-types";
import { setManualActivity } from "@/presence/self-activity";
import { $ownNotificationsSuppressed } from "@/presence/presence-store";
import type { VCard4Profile } from "@/lib/xmpp/vcard4-types";
import { installMessageToolbarLifecycleSuppression } from "@/stores/message-toolbar";
import {
  createBrowserIncomingCallNotifier,
  createBrowserLoopingTonePlayer,
  createIncomingCallAlertController,
} from "@/shell/audio-alerts";

const props = defineProps<{
  controller: ChatAppController;
}>();

const controller = props.controller;

const {
  connectionStore,
  ui,
  waddles,
  messaging,
  dmMessaging,
  dmConversations,
  channelUnread,
  rosterContacts,
  socialFeed,
  stories,
  communityEvents,
  communityJid,
  xmppClient,
  notifySettings,
  activeMessages,
  activeRoomChannel,
  activeFirstUnseenId,
  channelExtensionRoutes,
  activeExtensionRouteKey,
  activeExtensionRoute,
  activeChannelRoomJid,
  activeThreadStack,
  activeThreadTargetMessageId,
  activeThreadTargetRequestId,
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
  authorJidByNick,
  mentionCandidates,
  displayedMemberCount,
  displayedMemberState,
  activeDmPeer,
  computedChannelUnreadMap,
  groupDmConversations,
  notifications,
  appUpdate,
  version,
  avatarUrlByAuthor,
  authorHatsByNick,
  authorAuthorityByNick,
  activeActionError,
  activeRoomAccessRequirement,
  activeErrorActionLabel,
  activeUploadProgress,
  contentAreaRef,
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
  openRooms,
  openMembers,
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
  handleNewGroupDm,
  handleAddPeopleToDm,
  openCreateChannelDialog,
  openChannelEdit,
  sendActiveMessage,
  sendPublicChannelMessage,
  sendThreadMessage,
  sendCallChatMessage,
  sendGif,
  openThread,
  pushThreadFromStack,
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
  markActiveDisplayed,
  invokeActiveExtensionAction,
  invokeExtensionRouteAction,
  searchActiveMessages,
  clearActiveSearch,
  loadOlderActiveMessages,
  retryActiveLoad,
  ensureActiveMessageLoaded,
  loadOlderThreadMessages,
  pinActiveMessage,
  unpinActiveMessage,
  jumpToPinnedMessage,
} = props.controller;

/**
 * XEP-0272 Muji participant counts keyed by room JID. Derived from
 * the per-room nick list so the sidebar badge updates as occupants
 * join and leave the call without any extra round-trip. Computed
 * once here and threaded through RoomsPage so per-row lookups
 * stay O(1).
 */
const mucCallParticipantsStore = useStore($mucCallParticipants);
const mucCallTerminatePendingStore = useStore($mucCallTerminatePendingSessions);
const mucCallMediaStore = useStore($mucCallMedia);
const dmCallActivitiesStore = useStore($dmCallActivities);
const callStateStore = useStore($callState);
const activityGroupCallStarting = ref(false);
const callParticipantCounts = computed<Record<string, number>>(() => {
  return mucCallParticipantCounts(retainedMucCallParticipantsStore.value);
});
const activeChannelCallCount = computed(() => {
  return activeChannelRailCallCount(callParticipantCounts.value, callStateStore.value);
});
const activeDmCallCount = computed(() => {
  return activeDmRailCallCount(dmCallActivitiesStore.value, callStateStore.value);
});
const managedMucDomain = computed(() =>
  normalizeMucServiceDomain(waddles.mucServiceJid.value) || (selfDomain.value ? `muc.${selfDomain.value}` : ""),
);
const selfFullJid = computed(() => getSelfFullJid() ?? null);
const retainedMucCallParticipantsStore = computed<Record<string, string[]>>(() => {
  const next: Record<string, string[]> = {};
  for (const [roomJid, nicks] of Object.entries(mucCallParticipantsStore.value)) {
    next[roomJid] = [...nicks];
  }
  const self = selfFullJid.value;
  const nick = connectionStore.session?.username;
  if (!self || !nick) return next;
  for (const session of Object.values(mucCallTerminatePendingStore.value)) {
    if (!session.terminatePending || session.selfFullJid !== self) continue;
    const current = next[session.roomJid] ?? [];
    if (!current.includes(nick)) {
      next[session.roomJid] = [...current, nick];
    }
  }
  return next;
});
const retainedMucCallMediaStore = computed<Record<string, CallMedia>>(() => {
  const next: Record<string, CallMedia> = { ...mucCallMediaStore.value };
  const self = selfFullJid.value;
  if (!self) return next;
  for (const session of Object.values(mucCallTerminatePendingStore.value)) {
    if (!session.terminatePending || session.selfFullJid !== self || !session.media) continue;
    if (!next[session.roomJid]) next[session.roomJid] = session.media;
  }
  return next;
});

const homeDashboardProps = computed(() => buildHomeDashboardProps({
  spaces: waddles.sortedSpaces.value,
  channels: waddles.sortedChannels.value,
  contacts: rosterContacts.contacts.value,
  isLoading: waddles.isLoadingStructure.value || rosterContacts.isLoadingContacts.value,
  channelUnreadMap: computedChannelUnreadMap.value,
  mentionedRoomJids: messaging.mentionedChannelCounts.value,
  activeChannelJids: messaging.activeChannels.value,
  dmConversations: dmConversations.conversations.value,
  groupDms: groupDmConversations?.value ?? [],
  callParticipantCounts: callParticipantCounts.value,
  callParticipants: retainedMucCallParticipantsStore.value,
  callMediaByRoom: retainedMucCallMediaStore.value,
  dmCallActivities: dmCallActivitiesStore.value,
  managedMucDomain: managedMucDomain.value,
  selfFullJid: selfFullJid.value,
}));

const contentPaneClass = computed(() => {
  if (activeRightPanel.value === "thread") {
    if (activeThreadStack.value.length === 0) return "";
    return activeThreadStack.value.length === 1
      ? "chat-content-pane--desktop-split"
      : "chat-content-pane--hidden";
  }
  if (ui.sidebarMode.value === "channels") {
    return activeRightPanel.value ? "chat-content-pane--desktop-split" : "";
  }
  return "";
});
const activeDmThreadEntries = computed<MessageThreadEntry[]>(() => {
  if (ui.sidebarMode.value !== "dms" || !activeDmPeer.value) return [];
  return [...threads.index.value.values()].sort((left, right) => right.lastTs.localeCompare(left.lastTs));
});
const threadPanelIsDm = computed(() => ui.sidebarMode.value === "dms" && !!activeDmPeer.value);
const threadPanelConversationActive = computed(() =>
  (ui.sidebarMode.value === "channels" || ui.sidebarMode.value === "dms") &&
  activeRightPanel.value === "thread" &&
  activeThreadStack.value.length >= 1
);

// ── Community shell glue ──────────────────────────────────────────────
/** True when the page cascade lands on the conversation workspace
 * (ContentArea + right-side panels) rather than a community page. */
const conversationPageActive = computed(() =>
  ui.activePage.value === "chat" && ui.activeCommunitySurface.value === null,
);
const canSearchMessages = computed(() =>
  conversationPageActive.value && (!!activeRoomChannel.value || !!activeDmPeer.value),
);
/** ContentArea renders `actionError` inline (with Retry) inside the
 * conversation workspace. Every other page has no renderer for it, so a
 * failure raised there (group-DM creation, reactions from a page, …)
 * surfaces as a danger toast instead of vanishing. */
watch(() => ui.actionError.value, (message) => {
  if (!message || conversationPageActive.value) return;
  toast({ id: "shell-action-error", tone: "danger", title: message });
});
/** The header Search button opens the MessageSearchPanel that
 * ContentArea owns, through its exposed handle. */
function openMessageSearch() {
  contentAreaRef.value?.openSearch();
}
function startHuddleFromHeader() {
  const channel = activeRoomChannel.value;
  const roomJid = activeChannelRoomJid.value;
  if (channel && roomJid) {
    const media: CallMedia = { audio: true, video: false };
    if (channel.isGroupDm) joinGroupDmCallFromActivity(roomJid, media);
    else joinChannelCallFromActivity(channel.id, roomJid, media);
    return;
  }
  openRooms();
}
const hasCallContext = computed(() => {
  const phase = callStateStore.value.phase;
  if (phase !== "idle" && phase !== "ended") return true;
  if (activeChannelCallCount.value > 0 || activeDmCallCount.value > 0) return true;
  if (Object.values(callParticipantCounts.value).some((count) => count > 0)) return true;
  return Object.keys(dmCallActivitiesStore.value).length > 0;
});
/** Home / Rooms / Members always get a context column; a conversation
 * only when there is call activity to show in it. */
const contextColumnVisible = computed(() => {
  const page = ui.activePage.value;
  if (page === "dashboard" || page === "rooms" || page === "members") return true;
  return conversationPageActive.value && hasCallContext.value;
});
const nextEvent = computed<CommunityEvent | null>(() => {
  const first = sortEventsUpcomingFirst(communityEvents.events.value)[0];
  return first && isEventUpcomingOrOngoing(first) ? first : null;
});
const nextEventWhen = computed(() => {
  const event = nextEvent.value;
  const bounds = event ? eventBounds(event) : null;
  if (!bounds) return "";
  const date = new Date(bounds.startMs);
  return bounds.isAllDay
    ? date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" })
    : date.toLocaleString(undefined, { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
});
/** Contacts and DM peers who are available or busy right now, one per
 * bare JID. Honest "around now" for the Home context column — nobody
 * has a join date, so there is no "new this week". */
const aroundNow = computed(() => {
  const seen = new Set<string>();
  const people: { jid: string; name: string; avatarUrl: string | null; presence: "online" | "dnd" }[] = [];
  for (const conversation of dmConversations.conversations.value) {
    if (conversation.mucPm) continue;
    const presence = presenceFromShow(conversation.presenceShow);
    if (presence !== "online" && presence !== "dnd") continue;
    const jid = barePeerJid(conversation.peerJid).toLowerCase();
    if (seen.has(jid)) continue;
    seen.add(jid);
    people.push({ jid, name: conversation.peerUsername || jid, avatarUrl: conversation.peerAvatarUrl ?? null, presence });
  }
  for (const contact of rosterContacts.contacts.value) {
    const presence = presenceFromShow(contact.presenceShow);
    if (presence !== "online" && presence !== "dnd") continue;
    const jid = barePeerJid(contact.jid).toLowerCase();
    if (seen.has(jid)) continue;
    seen.add(jid);
    people.push({ jid, name: contact.name || contact.username || jid, avatarUrl: null, presence });
  }
  return people;
});
/** Member picked on the Members page; MembersPage emits null on unmount. */
const selectedMember = ref<MemberCardModel | null>(null);
function onSelectMember(member: MemberCardModel | null) {
  selectedMember.value = member;
}

function onCommunityRsvp(
  event: { uid: string },
  partstat: "ACCEPTED" | "DECLINED" | "TENTATIVE" | "NEEDS-ACTION",
) {
  const session = connectionStore.session;
  if (!session) return;
  const bareJid = barePeerJid(session.jid);
  const localpart = jidLocalpart(bareJid);
  if (!localpart) return;
  void communityEvents.rsvp(event.uid, localpart, bareJid, partstat);
}

function selectCurrentChannelExtensionRoute(route: DiscoveredExtensionRoute) {
  const channel = waddles.currentChannel.value;
  if (!channel) return;
  void selectExtensionRoute(channel.id, route);
}

function setPinnedPanelOpen(isOpen: boolean) {
  if (!isOpen) {
    closePinnedPanel();
    return;
  }
  ui.showPinnedPanel.value = true;
  activateRightPanel("pinned");
}

function onSelectCommunitySurface(surface: "feed" | "events") {
  openCommunitySurface(surface);
}

function onSelectChannelFromSidebar(id: string | null, roomJid?: string) {
  ui.activeCommunitySurface.value = null;
  if (id) {
    selectChannel(id, roomJid ? { roomJid } : undefined);
    return;
  }
  if (roomJid) selectChannelByRoomJid(roomJid);
}

async function lookupActiveConversationLinkPreview(body: string) {
  const client = xmppClient.value;
  const scope = threadPanelIsDm.value
    ? activeDmPeer.value?.peerJid
    : activeChannelRoomJid.value;
  if (!client || !scope) return null;
  return client.lookupLinkPreview(body, scope);
}

function requireFeedPepClient() {
  const client = xmppClient.value;
  if (!client) {
    throw new Error("Reconnect before publishing this update.");
  }
  return client;
}

async function refreshFeedAfterPepPublish(): Promise<void> {
  await socialFeed.refresh();
}

async function publishFeedPost(input: FeedPostInput): Promise<void> {
  const entry = await socialFeed.post(input);
  if (!entry) {
    throw new Error(socialFeed.error.value ?? "Couldn't publish post.");
  }
}

async function publishFeedStory(input: StoryPostInput): Promise<void> {
  const story = await stories.post(input);
  if (!story) {
    throw new Error(stories.error.value ?? "Couldn't publish story.");
  }
}

async function publishFeedMood(input: MoodPublication): Promise<void> {
  await requireFeedPepClient().publishMood(input);
  await refreshFeedAfterPepPublish();
}

async function publishFeedActivity(input: ActivityPublication): Promise<void> {
  // The activity node is owned by the ActivityCoordinator (so the in-call
  // overlay and manual activity don't clobber each other); record the intent
  // and the coordinator publishes synchronously before the feed refresh.
  // `requireFeedPepClient` keeps the connected-guard error UX.
  requireFeedPepClient();
  setManualActivity(input);
  await refreshFeedAfterPepPublish();
}

async function publishFeedTune(input: TunePublication): Promise<void> {
  await requireFeedPepClient().publishTune(input);
  await refreshFeedAfterPepPublish();
}

async function fetchFeedProfile(): Promise<VCard4Profile | null> {
  const selfJid = connectionStore.session?.jid;
  if (!selfJid) {
    throw new Error("Reconnect before loading your profile.");
  }
  return requireFeedPepClient().fetchVCard4(selfJid);
}

async function publishFeedProfile(input: VCard4Profile): Promise<void> {
  await requireFeedPepClient().publishVCard4(input);
  await refreshFeedAfterPepPublish();
}

function getCallSender(): CallWireSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

function getMucCallSender(): RawIqSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as RawIqSender | undefined) ?? null;
}

function getSelfFullJid(): string | undefined {
  return connectionStore.selfFullJid ??
    (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid;
}

function getExpectedMixerJid(): string | undefined {
  const accountJid = connectionStore.session?.jid;
  return accountJid ? `calls.${jidDomain(accountJid)}` : undefined;
}

function getClientJoiner(): ((roomJid: string) => Promise<void>) | null {
  const client = connectionStore.client as unknown as {
    ensureJoined?: (roomJid: string) => Promise<void>;
  } | null;
  return client?.ensureJoined?.bind(client) ?? null;
}

function isGroupCallBusy(): boolean {
  const current = $callState.get();
  return activityGroupCallStarting.value || (current.phase !== "idle" && current.phase !== "ended");
}

function reconnectDmFromDock(peerJid: string, _media: CallMedia): void {
  selectDm(peerJid);
  resumeDmCallActivity({ peerBareJid: peerJid, getSelfFullJid });
}

function answerDmFromActivity(peerJid: string, remoteFullJid: string, sid: string, media: CallMedia): void {
  selectDm(peerJid);
  void answerIncomingDmCallActivity({
    peerBareJid: peerJid,
    proposerFullJid: remoteFullJid,
    sid,
    media,
    getSender: getCallSender,
  });
}

function endRecoveredDmFromActivity(peerJid: string, sid?: string): void {
  selectDm(peerJid);
  void endRecoveredDmCallAction({
    peerBareJid: peerJid,
    sid,
    getSender: getCallSender,
    getSelfFullJid,
  });
}

function joinChannelCallFromActivity(channelId: string | null, roomJid: string, media: CallMedia): void {
  ui.activeCommunitySurface.value = null;
  if (channelId) {
    void selectChannel(channelId, { roomJid });
  } else {
    void selectChannelByRoomJid(roomJid);
  }
  void startMucCallAction({
    roomJid,
    media,
    isBusy: isGroupCallBusy,
    setStarting: (next) => {
      activityGroupCallStarting.value = next;
    },
    getSender: getMucCallSender,
    getSelfNick: () => connectionStore.session?.username ?? undefined,
    getSelfFullJid,
    getExpectedMixerJid,
    ensureJoined: async () => {
      await getClientJoiner()?.(roomJid);
    },
    // Rejoining a call we were already in: prefer LiveKit-direct
    // reconnect via the cached join over a fresh Jingle attempt.
    // LK identity-uniqueness displaces any orphan session cleanly.
    tryResumeFirst: true,
  });
}

function joinGroupDmCallFromActivity(roomJid: string, media: CallMedia): void {
  ui.activeCommunitySurface.value = null;
  void (async () => {
    const selected = await selectGroupDm(roomJid);
    if (selected === false) return;
    await startMucCallAction({
      roomJid,
      media,
      isBusy: isGroupCallBusy,
      setStarting: (next) => {
        activityGroupCallStarting.value = next;
      },
      getSender: getMucCallSender,
      getSelfNick: () => connectionStore.session?.username ?? undefined,
      getSelfFullJid,
      getExpectedMixerJid,
      ensureJoined: async () => {
        await getClientJoiner()?.(roomJid);
      },
      tryResumeFirst: true,
    });
  })();
}

function leaveRetainedChannelCall(roomJid: string): void {
  void leaveRetainedMucCallAction({
    roomJid,
    getSender: getMucCallSender,
    getSelfNick: () => connectionStore.session?.username ?? undefined,
    getSelfFullJid,
  });
}

// ── Admin route plumbing ────────────────────────────────────────────
// The admin view is rendered straight out of ChatReadyShell rather
// than from a per-route page Vue island, so the panel slug is
// derived directly from the reactive route match. `navigate()`
// updates the match synchronously, so panel switches re-render this
// tick without any manual pushState / popstate plumbing.
const routeMatch = useRouteMatch();
const adminPanelFromUrl = computed<AdminPanel>(() =>
  routeMatch.value.id === "admin" ? routeMatch.value.params.panel : "users",
);
function onAdminNavigate(match: AdminMatch) {
  navigate(match);
}
function onAdminBack() {
  openHome();
}
let disconnectMessageToolbarLifecycle: (() => void) | null = null;

onMounted(() => {
  disconnectMessageToolbarLifecycle = installMessageToolbarLifecycleSuppression();
  configureIncomingCallAlerts(createIncomingCallAlertController({
    player: createBrowserLoopingTonePlayer(),
    notifier: createBrowserIncomingCallNotifier(),
    focusTarget: {
      focusConversation(peerJid) {
        selectDm(peerJid);
      },
    },
    isTabFocused: () => document.visibilityState === "visible" && document.hasFocus(),
    // Presence Do Not Disturb silences this device's own incoming-call ringtone
    // + OS banner, reusing the same signal as the message-notification gate
    // (ADR-010 Phase 5a / #1075, #1081). The in-app IncomingCallToast is
    // `$callState`-driven, so the call stays visible/answerable — only the
    // disturbance is suppressed. Read at ring time via `.get()`.
    isDoNotDisturb: () => $ownNotificationsSuppressed.get(),
  }));
});

watch(selfFullJid, (fullJid) => {
  hydrateMucCallTerminatePendingSessions(fullJid);
}, { immediate: true });

onUnmounted(() => {
  configureIncomingCallAlerts(null);
  disconnectMessageToolbarLifecycle?.();
  disconnectMessageToolbarLifecycle = null;
});

/**
 * Superseded recovery outside the conversation surface: `ContentArea` owns
 * the connection banner, but dashboard / feed / events / threads / unread /
 * settings are sibling branches that render no recovery affordance while the
 * sticky superseded latch rejects every automatic reconnect. Surface the
 * explicit Reconnect action at the shell level for exactly those branches.
 */
const supersededOutsideConversation = computed(() => {
  if (messaging.xmppStatus.value.kind !== "superseded") return false;
  return (
    ui.activePage.value === "dashboard"
    || ui.activeCommunitySurface.value === "feed"
    || ui.activeCommunitySurface.value === "events"
    || ui.activePage.value === "threads"
    || ui.activePage.value === "unread"
    || ui.activePage.value === "rooms"
    || ui.activePage.value === "members"
    || ui.activePage.value === "settings"
  );
});

/**
 * Open right-side panels hide `ContentArea`'s own banner: ANY active right
 * panel (thread, pinned, extension) hides the content pane on mobile via
 * the desktop-split class, and a nested thread hides it entirely on both.
 * Whenever the desktop split still shows ContentArea, the shell banner is
 * `md:hidden` to avoid doubling it there.
 */
const supersededThreadStackDepth = computed(() =>
  activeRightPanel.value === "thread" ? activeThreadStack.value.length : 0,
);
const supersededContentPaneObscured = computed(() => activeRightPanel.value !== null);
const supersededBannerVisible = computed(() =>
  messaging.xmppStatus.value.kind === "superseded"
  && (supersededOutsideConversation.value || supersededContentPaneObscured.value),
);
const supersededBannerClass = computed(() =>
  !supersededOutsideConversation.value
  && supersededContentPaneObscured.value
  && supersededThreadStackDepth.value < 2
    ? "md:hidden"
    : "",
);

async function recoverSupersededFromShell() {
  await xmppClient.value?.recoverSupersededSession();
}
</script>

<template>
  <div v-if="ui.activePage.value === 'admin'" class="flex h-full min-h-0 flex-col">
    <!-- The admin route bypasses the chat shell entirely, so the superseded
         recovery affordance must render here as well or the sticky latch
         leaves the tab without any reconnect action. -->
    <SupersededRecoveryBanner
      v-if="messaging.xmppStatus.value.kind === 'superseded'"
      :detail="messaging.xmppStatus.value.detail"
      @recover="recoverSupersededFromShell"
    />
    <AdminView
      class="min-h-0 flex-1"
      :xmpp-client="xmppClient"
      :active-panel="adminPanelFromUrl"
      @navigate="onAdminNavigate"
      @back="onAdminBack"
    />
  </div>
  <div v-else class="chat-app-shell">
    <CallAudioPlaybackPrompt />
    <ChatMobileDrawers
      :controller="controller"
      :active-channel-call-count="activeChannelCallCount"
      :active-dm-call-count="activeDmCallCount"
      :call-participant-counts="callParticipantCounts"
      :call-participants="retainedMucCallParticipantsStore"
      :call-media-by-room="retainedMucCallMediaStore"
    />
    <CurrentCallPanel
      class="current-call-panel--mobile"
      :channels="waddles.sortedChannels.value"
      :conversations="dmConversations.conversations.value"
      :active-channel-id="waddles.activeChannelId.value"
      :active-channel-room-jid="activeChannelRoomJid"
      :active-peer-jid="dmConversations.activePeerJid.value"
      @select-channel="onSelectChannelFromSidebar"
      @select-dm="selectDm"
    />
    <CallActivityDock
      class="call-activity-dock--mobile"
      :channels="waddles.sortedChannels.value"
      :group-dms="groupDmConversations"
      :conversations="dmConversations.conversations.value"
      :active-channel-id="waddles.activeChannelId.value"
      :active-channel-room-jid="activeChannelRoomJid"
      :active-peer-jid="dmConversations.activePeerJid.value"
      :sidebar-mode="ui.sidebarMode.value"
      :active-channel-jids="messaging.activeChannels.value"
      :call-participants="retainedMucCallParticipantsStore"
      :call-media-by-room="retainedMucCallMediaStore"
      :managed-muc-domain="managedMucDomain"
      :self-full-jid="selfFullJid"
      hide-current-call
      @select-channel="onSelectChannelFromSidebar"
      @select-group-dm="selectGroupDm"
      @join-channel-call="joinChannelCallFromActivity"
      @join-group-dm-call="joinGroupDmCallFromActivity"
      @leave-channel-call="leaveRetainedChannelCall"
      @answer-dm="answerDmFromActivity"
      @select-dm="selectDm"
      @reconnect-dm="reconnectDmFromDock"
      @end-dm="endRecoveredDmFromActivity"
    />

    <!-- Community shell: header + people rail + page + context column -->
    <CommunityShell
      :controller="controller"
      :hide-rail="ui.activePage.value === 'settings'"
      :can-search="canSearchMessages"
      :open-search="openMessageSearch"
      :start-huddle="startHuddleFromHeader"
      :call-participants="retainedMucCallParticipantsStore"
    >
      <!-- Superseded recovery: the shell's main column is a flex column,
           so an in-flow banner would push the active page down. Pin it
           across the full column width instead; its own background/border
           keep it readable over any surface. -->
      <div
        v-if="supersededBannerVisible"
        class="absolute inset-x-0 top-0 z-40"
        :class="supersededBannerClass"
      >
        <SupersededRecoveryBanner
          :detail="messaging.xmppStatus.value.detail"
          @recover="recoverSupersededFromShell"
        />
      </div>
      <!-- Main content -->
      <HomeDashboard
        v-if="ui.activePage.value === 'dashboard'"
        v-bind="homeDashboardProps"
        @select-channel="(id: string, roomJid?: string) => selectChannel(id, roomJid ? { roomJid } : undefined)"
        @select-channel-room="selectChannelByRoomJid"
        @select-group-dm="selectGroupDm"
        @join-channel-call="joinChannelCallFromActivity"
        @join-group-dm-call="joinGroupDmCallFromActivity"
        @leave-channel-call="leaveRetainedChannelCall"
        @answer-dm="answerDmFromActivity"
        @select-contact="handleOpenDm"
        @reconnect-dm="reconnectDmFromDock"
        @end-dm="endRecoveredDmFromActivity"
        @open-nav="ui.showMobileNav.value = true"
      />
      <FeedPane
        v-else-if="ui.activeCommunitySurface.value === 'feed'"
        :entries="socialFeed.entries.value"
        :stories="stories.activeStories.value"
        :is-loading="socialFeed.isLoading.value"
        :is-stories-loading="stories.isLoading.value"
        :is-posting="socialFeed.isPosting.value"
        :is-story-posting="stories.isPosting.value"
        :error="socialFeed.error.value"
        :stories-error="stories.error.value"
        :can-post="!!connectionStore.session"
        :self-jid="connectionStore.session?.jid ?? null"
        :is-story-read="stories.isStoryRead"
        :reaction-summary="stories.reactionSummary"
        :initial-filter="ui.feedDefaultFilter.value"
        :initial-composer-mode="ui.feedDefaultComposerMode.value"
        :publish-post="publishFeedPost"
        :publish-story="publishFeedStory"
        :publish-mood="publishFeedMood"
        :publish-activity="publishFeedActivity"
        :publish-tune="publishFeedTune"
        :fetch-profile="fetchFeedProfile"
        :publish-profile="publishFeedProfile"
        @refresh="socialFeed.refresh(); stories.refresh()"
        @story-selected="(id) => stories.markStoryRead(id)"
        @react="(id, emoji) => stories.toggleReaction(id, emoji)"
        @open-nav="ui.showMobileNav.value = true"
      />
      <EventsPane
        v-else-if="ui.activeCommunitySurface.value === 'events'"
        :events="communityEvents.events.value"
        :is-loading="communityEvents.isLoading.value"
        :is-posting="communityEvents.isPosting.value"
        :error="communityEvents.error.value"
        :can-post="!!connectionStore.session"
        :self-jid="connectionStore.session?.jid ?? null"
        :community-jid="communityJid"
        :server-base-url="connectionStore.activeServerUrl"
        :session-id="connectionStore.session?.session_id ?? null"
        :find-master="communityEvents.findMaster"
        @refresh="communityEvents.refresh()"
        @post="(input) => communityEvents.post(input)"
        @edit="(id, input) => communityEvents.edit(id, input)"
        @cancel-series="(id) => communityEvents.cancel(id)"
        @cancel-instance="(uid, dtstart) => communityEvents.cancelInstance(uid, dtstart)"
        @rsvp="(event, partstat) => onCommunityRsvp(event, partstat)"
        @open-nav="ui.showMobileNav.value = true"
      />
      <ThreadsView
        v-else-if="ui.activePage.value === 'threads'"
        :channels="waddles.sortedChannels.value"
        :on-select-thread-entry="onSelectThreadEntry"
        :on-join-channel-call="joinChannelCallFromActivity"
        @open-nav="ui.showMobileNav.value = true"
      />
      <UnreadView
        v-else-if="ui.activePage.value === 'unread'"
        :channels="waddles.sortedChannels.value"
        :inbox-state="channelUnread.inboxState.value"
        :on-select-channel="(id: string) => selectChannel(id)"
        :on-select-thread="onSelectThread"
        :on-refresh-inbox="channelUnread.hydrateFromInbox"
        @open-nav="ui.showMobileNav.value = true"
      />
      <RoomsPage
        v-else-if="ui.activePage.value === 'rooms'"
        :controller="controller"
        :call-participant-counts="callParticipantCounts"
        :call-participants="retainedMucCallParticipantsStore"
        :call-media-by-room="retainedMucCallMediaStore"
        :managed-muc-domain="managedMucDomain"
        :self-full-jid="selfFullJid"
        :dm-thread-entries="activeDmThreadEntries"
        :join-channel-call="joinChannelCallFromActivity"
        :leave-channel-call="leaveRetainedChannelCall"
        :answer-dm="answerDmFromActivity"
        :reconnect-dm="reconnectDmFromDock"
        :end-dm="endRecoveredDmFromActivity"
        @open-nav="ui.showMobileNav.value = true"
      />
      <MembersPage
        v-else-if="ui.activePage.value === 'members'"
        :controller="controller"
        :call-participants="retainedMucCallParticipantsStore"
        @select-member="onSelectMember"
        @open-nav="ui.showMobileNav.value = true"
      />
      <UserSettingsPage
        v-else-if="ui.activePage.value === 'settings' && connectionStore.session"
        :session="connectionStore.session"
        :xmpp-client="xmppClient"
        :message-sounds-enabled="notifications.messageSoundsEnabled.value"
        :web-commit-sha="version.webCommitSha.value"
        :server-version="version.serverVersion.value"
        @close="closeUserSettings"
        @toggle-message-sounds="handleToggleMessageSounds"
      />
      <template v-else>
        <!--
          Accordion thread layout
          ---------------------------------------------------------------------
          Stack depth 0  -> ContentArea takes full remaining width
          Stack depth 1  -> ContentArea (desktop left pane) + active ThreadPanel
          Stack depth 2+ -> parent ThreadPanel (desktop, read-only) + active ThreadPanel

          On mobile only the active thread panel is visible when a thread is open.
          Escape key (handled above) pops back one level in the stack.
        -->

        <!-- ContentArea wrapper:
             - depth 0: visible and flex-1 (full remaining width)
             - depth 1: hidden on mobile, flex-1 on desktop (left pane)
             - depth 2+: hidden entirely (parent thread pane takes its place) -->
        <div
          class="chat-workspace"
          :class="activeRightPanel ? 'chat-workspace--right-panel-active' : ''"
        >
          <div
            :class="[
              'chat-content-pane',
              contentPaneClass,
            ]"
          >
            <ContentArea
              :ref="setContentAreaRef"
              v-model:draft="activeDraft"
              v-model:forum-title="activeForumTitle"
              :pinned-panel-open="activeRightPanel === 'pinned' && ui.showPinnedPanel.value"
              :waddle="waddles.currentSpace.value"
              :channel="activeRoomChannel"
              :room-jid="activeRoomChannel ? activeChannelRoomJid : null"
              :dm-peer="activeDmPeer"
              :sidebar-mode="ui.sidebarMode.value"
              :messages="activeMessages"
              :first-unseen-id="activeFirstUnseenId"
              :xmpp-status="messaging.xmppStatus.value"
              :action-error="activeActionError"
              :channel-access-required="!!activeRoomAccessRequirement"
              :error-action-label="activeErrorActionLabel"
              :update-available="appUpdate.updateAvailable.value"
              :is-applying-update="appUpdate.isApplyingUpdate.value"
              :is-loading-messages="contentAreaIsLoadingMessages"
              :is-loading-older-messages="activeIsLoadingOlderMessages"
              :has-older-messages="activeHasOlderMessages"
              :is-sending="activeIsSending"
              :can-manage-channels="waddles.canManageChannels.value"
              :member-count="displayedMemberCount"
              :member-state="displayedMemberState"
              :typing-users="activeTypingUsers"
              :current-user="connectionStore.session?.username"
              :current-user-jid="connectionStore.session?.jid"
              :self-full-jid="selfFullJid"
              :self-domain="selfDomain"
              :avatar-url-by-author="avatarUrlByAuthor"
              :author-jid-by-nick="authorJidByNick"
              :mention-candidates="mentionCandidates"
              :room-hats="authorHatsByNick"
              :room-authority="authorAuthorityByNick"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :search-results="activeSearchResults"
              :is-searching="activeIsSearching"
              :upload-progress="activeUploadProgress"
              :thread-index="threads.index.value"
              :xmpp-client="xmppClient"
              :notify-settings="notifySettings"
              :reaction-mode="reactionModeTarget === 'main' ? reactionModeState : null"
              :ensure-message-loaded="ensureActiveMessageLoaded"
              :send-public-channel-message="sendPublicChannelMessage"
              :send-call-chat-message="sendCallChatMessage"
              @send="sendActiveMessage"
              @send-call-chat="sendThreadMessage"
              @typing="notifyActiveComposing"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              @pin-message="pinActiveMessage"
              @unpin-message="unpinActiveMessage"
              @update:pinned-panel-open="setPinnedPanelOpen"
              @search="searchActiveMessages"
              @clear-search="clearActiveSearch"
              @load-older="loadOlderActiveMessages"
              @retry-load="retryActiveLoad"
              @edit-channel="openChannelEdit"
              @open-nav="ui.showMobileNav.value = true"
              @open-details="ui.showMobileDetails.value = true"
              @open-dm="handleOpenDm"
              @open-thread="openThread"
              @join-channel-call="joinChannelCallFromActivity"
              @leave-channel-call="leaveRetainedChannelCall"
              @answer-dm="answerDmFromActivity"
              @reconnect-dm="reconnectDmFromDock"
              @end-dm="endRecoveredDmFromActivity"
              :invoke-extension-action="invokeActiveExtensionAction"
              @refresh-update="refreshAppUpdate"
            />
          </div>

          <!-- Collapsed accordion bars: one per inactive right-side panel. -->
          <button
            v-if="(ui.sidebarMode.value === 'channels' || ui.sidebarMode.value === 'dms')
              && activeRightPanel !== 'thread'
              && activeThreadStack.length >= 1"
            type="button"
            class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
            :title="getThreadLabel(activeThreadStack[activeThreadStack.length - 1] ?? '')"
            @click="activateRightPanel('thread')"
          >
            <span class="accordion-bar-label text-muted-foreground/70">
              {{ getThreadLabel(activeThreadStack[activeThreadStack.length - 1] ?? '') }}
            </span>
          </button>

          <template v-if="threadPanelConversationActive && activeThreadStack.length >= 2">
            <!-- Channel / main-feed bar -->
            <button
              type="button"
              class="chat-accordion-bar bg-muted/30 hover:bg-muted/60"
              title="Back to channel"
              @click="closeThreadPanel"
            >
              <span class="accordion-bar-label text-muted-foreground/80">
                {{ threadPanelIsDm ? (activeDmPeer?.peerUsername ?? 'Direct message') : (waddles.currentChannel.value?.name ?? 'Channel') }}
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

          <button
            v-if="(ui.sidebarMode.value === 'channels' || ui.sidebarMode.value === 'dms')
              && activeRightPanel !== 'pinned'
              && ui.showPinnedPanel.value
              && (activeChannelRoomJid || activeDmPeer?.peerJid)"
            type="button"
            class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
            title="Pinned messages"
            @click="activateRightPanel('pinned')"
          >
            <span class="accordion-bar-label text-muted-foreground/70">
              Pinned
            </span>
          </button>

          <button
            v-if="ui.sidebarMode.value === 'channels'
              && activeRightPanel !== 'extension'
              && activeExtensionRouteKey"
            type="button"
            class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
            :title="activeExtensionRoute?.label ?? 'Extensions'"
            @click="activateRightPanel('extension')"
          >
            <span class="accordion-bar-label text-muted-foreground/70">
              {{ activeExtensionRoute?.label ?? 'Extensions' }}
            </span>
          </button>

          <!-- Parent thread pane: desktop-only context when depth >= 2.
               Shows the second-to-last thread in read-only mode (no composer). -->
          <div
            v-if="threadPanelConversationActive && activeThreadStack.length >= 2"
            class="chat-parent-thread-pane"
          >
            <ThreadPanel
              :thread-stack="activeThreadStack.slice(0, -1)"
              :thread-index="threads.index.value"
              :resolve-entry="threads.resolveEntry"
              :current-user="connectionStore.session?.username"
              :current-user-jid="connectionStore.session?.jid"
              :avatar-url-by-author="avatarUrlByAuthor"
              :author-jid-by-nick="authorJidByNick"
              :room-hats="authorHatsByNick"
              :room-authority="authorAuthorityByNick"
              :room-presence="threadPanelIsDm ? {} : messaging.roomPresence.value"
              :room-last-seen="threadPanelIsDm ? {} : messaging.roomLastSeen.value"
              :mention-candidates="mentionCandidates"
              :slow-mode-cooldown="threadPanelIsDm ? 0 : messaging.slowModeCooldown.value"
              :is-sending="false"
              :is-loading-older-replies="(threadPanelIsDm ? dmMessaging.loadingOlderThreadIds.value : messaging.loadingOlderThreadIds.value).has(activeThreadStack[activeThreadStack.length - 2] ?? '')"
              :has-older-replies="(threadPanelIsDm ? dmMessaging.threadHasOlder.value : messaging.threadHasOlder.value)[activeThreadStack[activeThreadStack.length - 2] ?? ''] ?? false"
              :upload-progress="{ uploading: false, progress: 0, filename: '' }"
              :channel-name="threadPanelIsDm ? (activeDmPeer?.peerUsername ?? '') : (waddles.currentChannel.value?.name ?? '')"
              :channel-id="threadPanelIsDm ? null : (waddles.currentChannel.value?.id ?? null)"
              :room-jid="threadPanelIsDm ? null : activeChannelRoomJid"
              :hide-composer="true"
              :reaction-mode="null"
              :link-preview-lookup="lookupActiveConversationLinkPreview"
              :link-preview-scope="threadPanelIsDm ? activeDmPeer?.peerJid ?? null : activeChannelRoomJid"
              @close="closeThreadPanel"
              @pop-to="popThreadTo"
              @push-thread="(threadId) => pushThreadFromStack(activeThreadStack.slice(0, -1), threadId)"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              :invoke-extension-action="invokeActiveExtensionAction"
              @displayed="markActiveDisplayed"
              @load-older="loadOlderThreadMessages"
              @join-channel-call="joinChannelCallFromActivity"
            />
          </div>

          <!-- Active thread pane: shown when any thread is open.
               Full-width on mobile; shares space with parent context on desktop. -->
          <div
            v-if="threadPanelConversationActive"
            class="chat-active-thread-pane"
          >
            <ThreadPanel
              :thread-stack="activeThreadStack"
              :thread-index="threads.index.value"
              :resolve-entry="threads.resolveEntry"
              :current-user="connectionStore.session?.username"
              :current-user-jid="connectionStore.session?.jid"
              :avatar-url-by-author="avatarUrlByAuthor"
              :author-jid-by-nick="authorJidByNick"
              :room-hats="authorHatsByNick"
              :room-authority="authorAuthorityByNick"
              :room-presence="threadPanelIsDm ? {} : messaging.roomPresence.value"
              :room-last-seen="threadPanelIsDm ? {} : messaging.roomLastSeen.value"
              :mention-candidates="mentionCandidates"
              :slow-mode-cooldown="threadPanelIsDm ? 0 : messaging.slowModeCooldown.value"
              :is-sending="activeIsSending"
              :is-loading-older-replies="(threadPanelIsDm ? dmMessaging.loadingOlderThreadIds.value : messaging.loadingOlderThreadIds.value).has(activeThreadStack[activeThreadStack.length - 1] ?? '')"
              :has-older-replies="(threadPanelIsDm ? dmMessaging.threadHasOlder.value : messaging.threadHasOlder.value)[activeThreadStack[activeThreadStack.length - 1] ?? ''] ?? false"
              :upload-progress="activeUploadProgress"
              :channel-name="threadPanelIsDm ? (activeDmPeer?.peerUsername ?? '') : (waddles.currentChannel.value?.name ?? '')"
              :channel-id="threadPanelIsDm ? null : (waddles.currentChannel.value?.id ?? null)"
              :room-jid="threadPanelIsDm ? null : activeChannelRoomJid"
              :reaction-mode="reactionModeTarget === 'thread' ? reactionModeState : null"
              :target-message-id="activeThreadTargetMessageId"
              :target-message-request-id="activeThreadTargetRequestId"
              :link-preview-lookup="lookupActiveConversationLinkPreview"
              :link-preview-scope="threadPanelIsDm ? activeDmPeer?.peerJid ?? null : activeChannelRoomJid"
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
              @join-channel-call="joinChannelCallFromActivity"
            />
          </div>

          <div
            v-if="(ui.sidebarMode.value === 'channels' || ui.sidebarMode.value === 'dms')
              && activeRightPanel === 'pinned'
              && ui.showPinnedPanel.value
              && (activeChannelRoomJid || activeDmPeer?.peerJid)"
            class="chat-active-thread-pane"
          >
            <PinnedPanel
              :room-jid="threadPanelIsDm ? activeDmPeer?.peerJid ?? '' : activeChannelRoomJid ?? ''"
              :channel-name="threadPanelIsDm ? activeDmPeer?.peerUsername ?? 'Direct message' : waddles.currentChannel.value?.name ?? ''"
              :timeline-messages="activeMessages"
              @close="closePinnedPanel"
              @jump-to-message="(stanzaId: string) => jumpToPinnedMessage(stanzaId)"
            />
          </div>

          <div
            v-if="ui.sidebarMode.value === 'channels'
              && activeRightPanel === 'extension'
              && activeExtensionRouteKey"
            class="chat-active-thread-pane"
          >
            <ExtensionRouteView
              :waddle="waddles.currentSpace.value"
              :channel="waddles.currentChannel.value"
              :route="activeExtensionRoute"
              :requested-route="activeExtensionRouteKey"
              :room-jid="activeChannelRoomJid"
              :xmpp-client="xmppClient"
              :action-error="activeActionError"
              @open-nav="ui.showMobileNav.value = true"
              @close="closeExtensionRoutePanel"
              @invoke-action="invokeExtensionRouteAction"
            />
          </div>

          <ExtensionRouteRail
            v-if="ui.sidebarMode.value === 'channels' && waddles.currentChannel.value"
            :routes="channelExtensionRoutes"
            :active-route="activeExtensionRouteKey"
            :active="activeRightPanel === 'extension'"
            @select-route="selectCurrentChannelExtensionRoute"
          />
        </div>
      </template>

      <!-- Context column: the live thing that matters on this page. -->
      <template v-if="contextColumnVisible" #context>
        <div class="context-column">
          <template v-if="ui.activePage.value === 'dashboard'">
            <div class="context-card">
              <span class="community-kicker">Next event</span>
              <template v-if="nextEvent">
                <span class="context-card__title">{{ nextEvent.summary }}</span>
                <span class="context-card__text">{{ nextEventWhen }}</span>
                <button
                  type="button"
                  class="community-pill self-start"
                  @click="openCommunitySurface('events')"
                >
                  See events
                </button>
              </template>
              <span v-else class="context-card__text">Nothing scheduled yet. Events you post show up here.</span>
            </div>
            <div class="context-card">
              <span class="community-kicker">Around now · {{ aroundNow.length }}</span>
              <div v-if="aroundNow.length > 0" class="context-avatars">
                <button
                  v-for="person in aroundNow.slice(0, 12)"
                  :key="person.jid"
                  type="button"
                  class="rounded-lg"
                  :aria-label="`Message ${person.name}`"
                  @click="handleOpenDm(person.jid)"
                >
                  <AppAvatar :name="person.name" :src="person.avatarUrl" :presence="person.presence" size="md" />
                </button>
              </div>
              <span v-else class="context-card__text">Nobody around right now. Start a huddle and they will find you.</span>
            </div>
          </template>
          <template v-else-if="ui.activePage.value === 'members'">
            <MemberProfileCard
              v-if="selectedMember"
              :member="selectedMember"
              :xmpp-client="xmppClient"
              :is-self="!!connectionStore.session && barePeerJid(connectionStore.session.jid).toLowerCase() === selectedMember.jid"
              @message="handleOpenDm"
              @close="selectedMember = null"
            />
            <div v-else class="context-card">
              <span class="community-kicker">Profile</span>
              <span class="context-card__text">Pick a member to see their profile here.</span>
            </div>
          </template>
          <template v-if="ui.activePage.value !== 'members'">
            <CurrentCallPanel
              :channels="waddles.sortedChannels.value"
              :conversations="dmConversations.conversations.value"
              :active-channel-id="waddles.activeChannelId.value"
              :active-channel-room-jid="activeChannelRoomJid"
              :active-peer-jid="dmConversations.activePeerJid.value"
              @select-channel="onSelectChannelFromSidebar"
              @select-dm="selectDm"
            />
            <CallActivityDock
              :channels="waddles.sortedChannels.value"
              :group-dms="groupDmConversations"
              :conversations="dmConversations.conversations.value"
              :active-channel-id="waddles.activeChannelId.value"
              :active-channel-room-jid="activeChannelRoomJid"
              :active-peer-jid="dmConversations.activePeerJid.value"
              :sidebar-mode="ui.sidebarMode.value"
              :active-channel-jids="messaging.activeChannels.value"
              :call-participants="retainedMucCallParticipantsStore"
              :call-media-by-room="retainedMucCallMediaStore"
              :managed-muc-domain="managedMucDomain"
              :self-full-jid="selfFullJid"
              :show-dm-calls="ui.sidebarMode.value !== 'dms'"
              hide-current-call
              @select-channel="onSelectChannelFromSidebar"
              @select-group-dm="selectGroupDm"
              @join-channel-call="joinChannelCallFromActivity"
              @join-group-dm-call="joinGroupDmCallFromActivity"
              @leave-channel-call="leaveRetainedChannelCall"
              @answer-dm="answerDmFromActivity"
              @select-dm="selectDm"
              @reconnect-dm="reconnectDmFromDock"
              @end-dm="endRecoveredDmFromActivity"
            />
          </template>
        </div>
      </template>
    </CommunityShell>

    <ChatAppModals :controller="controller" />
  </div>
</template>
