import { type ComponentPublicInstance, computed, type Ref, ref, watchEffect } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import type { ScrollDirectionMode } from "@/lib/scroll-direction";

export type ContentAreaHandle = ComponentPublicInstance & {
  messagesContainer: HTMLDivElement | null;
  scrollToPinnedEdge: (mode: ScrollDirectionMode) => Promise<boolean>;
  scrollToMessage: (messageId: string) => Promise<void>;
};

interface ActiveConversationDeps {
  ui: ChatShellState;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  isApplyingRoute: Ref<boolean>;
}

/**
 * Fan-out over the active conversation surface: every `active*` computed
 * resolves to either the channel messaging composable or the DM messaging
 * composable depending on which surface (channels vs DMs) is showing, and
 * the ContentArea template ref is wired to whichever timeline is live.
 */
export function useActiveConversation(deps: ActiveConversationDeps) {
  const { ui, waddles, messaging, dmMessaging, dmConversations, isApplyingRoute } = deps;

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
  const activeUploadProgress = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging.uploadProgress.value : messaging.uploadProgress.value,
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
      presenceIdleSince: conversation.presenceIdleSince,
    };
  });

  const activeTarget = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging : messaging,
  );

  const activeRoomAccessRequirement = computed(() =>
    isActiveDirectDmSurface() ? null : messaging.currentRoomAccessRequirement.value,
  );
  const activeActionError = computed(() =>
    activeRoomAccessRequirement.value ? "" : ui.actionError.value,
  );
  const activeErrorActionLabel = computed(() => {
    const peer = activeDmPeer.value;
    return isActiveDirectDmSurface() &&
      peer &&
      dmMessaging.loadErrorPeerJid.value === peer.peerJid &&
      activeActionError.value === dmMessaging.loadErrorMessage.value
      ? "Try again"
      : null;
  });

  return {
    contentAreaRef,
    setContentAreaRef,
    isActiveDirectDmSurface,
    activeMessages,
    activeFirstUnseenId,
    activeDraft,
    activeForumTitle,
    activeTypingUsers,
    contentAreaIsLoadingMessages,
    activeIsLoadingOlderMessages,
    activeHasOlderMessages,
    activeIsSending,
    activeSearchResults,
    activeIsSearching,
    activeUploadProgress,
    activeDmPeer,
    activeTarget,
    activeRoomAccessRequirement,
    activeActionError,
    activeErrorActionLabel,
  };
}
