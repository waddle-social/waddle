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
 * View state comes from the selected room or direct-message conversation.
 * Without a conversation, the view is idle and neither timeline is bound
 * to the ContentArea template ref.
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
  const activeRoomChannel = computed(() => {
    if (isActiveDirectDmSurface()) return null;
    const channel = waddles.currentChannel.value;
    return ui.sidebarMode.value === "channels" || channel?.isGroupDm ? channel : null;
  });
  const activeTarget = computed(() =>
    isActiveDirectDmSurface() ? dmMessaging : activeRoomChannel.value ? messaging : null,
  );

  watchEffect(() => {
    const timeline = contentAreaRef.value?.messagesContainer ?? null;
    const edgeScroller = contentAreaRef.value?.scrollToPinnedEdge ?? null;
    for (const target of [messaging, dmMessaging]) {
      const active = target === activeTarget.value;
      target.timelineEl.value = active ? timeline : null;
      target.timelineEdgeScroller.value = active ? edgeScroller : null;
    }
  });

  const activeMessages = computed(() =>
    activeTarget.value?.messages.value ?? [],
  );
  const activeFirstUnseenId = computed(() =>
    activeTarget.value?.firstUnseenId.value ?? null,
  );

  const activeDraft = computed({
    get: () => activeTarget.value?.draft.value ?? "",
    set: (value: string) => {
      if (activeTarget.value) activeTarget.value.draft.value = value;
    },
  });
  const activeForumTitle = computed({
    get: () => activeRoomChannel.value ? messaging.forumPostTitle.value : "",
    set: (value: string) => {
      if (activeRoomChannel.value) {
        messaging.forumPostTitle.value = value;
      }
    },
  });
  const activeTypingUsers = computed(() =>
    activeTarget.value?.typingUsers.value ?? [],
  );
  const activeIsLoadingMessages = computed(() =>
    activeTarget.value?.isLoadingMessages.value ?? false,
  );
  const isResolvingActiveConversation = computed(() =>
    ui.activePage.value === "chat"
    && !activeTarget.value
    && isApplyingRoute.value,
  );
  const contentAreaIsLoadingMessages = computed(() =>
    activeIsLoadingMessages.value || isResolvingActiveConversation.value,
  );
  const activeIsLoadingOlderMessages = computed(() =>
    activeTarget.value?.isLoadingOlderMessages.value ?? false,
  );
  const activeHasOlderMessages = computed(() =>
    activeTarget.value?.hasOlderMessages.value ?? false,
  );
  const activeIsSending = computed(() =>
    activeTarget.value?.isSending.value ?? false,
  );
  const activeSearchResults = computed(() =>
    activeTarget.value?.searchResults.value ?? [],
  );
  const activeIsSearching = computed(() =>
    activeTarget.value?.isSearching.value ?? false,
  );
  const activeUploadProgress = computed(() =>
    activeTarget.value?.uploadProgress.value ?? { uploading: false, progress: 0, filename: "" },
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

  const activeRoomAccessRequirement = computed(() =>
    activeRoomChannel.value ? messaging.currentRoomAccessRequirement.value : null,
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
    activeRoomChannel,
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
