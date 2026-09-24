import { describe, expect, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useActiveConversation } from "../src/shell/controllers/use-active-conversation";
import type { ChannelSummary } from "../src/lib/chat-types";
import { useChannelMamPaging } from "../src/channels/mam-paging";
import type { ContentAreaHandle } from "../src/shell/controllers/use-active-conversation";

function messageState(prefix: string) {
  return {
    messages: ref([{ id: `${prefix}-message` }]),
    firstUnseenId: ref<string | null>(`${prefix}-unseen`),
    draft: ref(`${prefix}-draft`), forumPostTitle: ref(`${prefix}-title`),
    typingUsers: ref([`${prefix}-sender`]),
    isLoadingMessages: ref(true), isLoadingOlderMessages: ref(true), hasOlderMessages: ref(true),
    isSending: ref(true), searchResults: ref([{ id: `${prefix}-search` }]), isSearching: ref(true),
    uploadProgress: ref({ uploading: true, progress: 40, filename: `${prefix}.png` }),
    timelineEl: ref<HTMLElement | null>(null), timelineEdgeScroller: ref<unknown>(null),
    currentRoomAccessRequirement: ref(null),
    loadErrorPeerJid: ref(null), loadErrorMessage: ref(""),
  };
}

function displayedState(conversation: ReturnType<typeof useActiveConversation>) {
  return {
    messages: conversation.activeMessages.value,
    firstUnseenId: conversation.activeFirstUnseenId.value,
    draft: conversation.activeDraft.value,
    forumTitle: conversation.activeForumTitle.value,
    typingUsers: conversation.activeTypingUsers.value,
    loading: conversation.contentAreaIsLoadingMessages.value,
    loadingOlder: conversation.activeIsLoadingOlderMessages.value,
    hasOlder: conversation.activeHasOlderMessages.value,
    sending: conversation.activeIsSending.value,
    searchResults: conversation.activeSearchResults.value,
    searching: conversation.activeIsSearching.value,
    upload: conversation.activeUploadProgress.value,
  };
}

describe("active conversation surface", () => {
  test("an empty DM view stays idle while channel history and directory work are pending", async () => {
    const ui = useChatShellState();
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "channels";
    const currentChannel = ref<ChannelSummary | null>({ id: "general", name: "General", jid: "general@muc.example.com" });
    const messaging = messageState("room");
    const dmMessaging = messageState("dm");
    let finishHistory!: (page: { messages: []; firstArchiveId: null; complete: true }) => void;
    const history = new Promise<{ messages: []; firstArchiveId: null; complete: true }>((resolve) => { finishHistory = resolve; });
    const scope = effectScope();
    const paging = useChannelMamPaging({
      session: ref({ jid: "alice@example.com", username: "alice" }),
      xmppClient: ref({ queryMamPage: () => history }),
      activeSpaceId: ref(null), activeChannelId: ref("general"), currentChannel,
      messages: messaging.messages, firstUnseenId: messaging.firstUnseenId,
      timelineEl: messaging.timelineEl, scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock() {} },
      actionError: ui.actionError, clearActionError: ui.clearActionError, normalizeError: String,
      pendingEchoClientIds: new Set(), appendQueuedMessages: (messages: unknown[]) => messages,
      roomJidForChannel: () => "general@muc.example.com", isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => false, persistLastSeen() {},
    } as unknown as Parameters<typeof useChannelMamPaging>[0]);
    Object.assign(messaging, paging);
    const isApplyingRoute = ref(false);
    const conversation = scope.run(() => useActiveConversation({
      ui, waddles: { currentChannel, isLoadingStructure: ref(true) }, messaging, dmMessaging,
      dmConversations: { activePeerJid: ref(null), conversations: ref([]) }, isApplyingRoute,
    } as unknown as Parameters<typeof useActiveConversation>[0]))!;
    const pendingLoad = paging.loadMessages("", "general");
    try {
      expect(paging.isLoadingMessages.value).toBe(true);
      expect(conversation.contentAreaIsLoadingMessages.value).toBe(true);
      ui.sidebarMode.value = "dms";
      conversation.setContentAreaRef({ messagesContainer: {}, scrollToPinnedEdge: async () => true } as unknown as ContentAreaHandle);
      await nextTick();

      const emptyState = {
        messages: [], firstUnseenId: null, draft: "", forumTitle: "", typingUsers: [],
        loading: false, loadingOlder: false, hasOlder: false, sending: false,
        searchResults: [], searching: false,
        upload: { uploading: false, progress: 0, filename: "" },
      };
      expect(displayedState(conversation)).toEqual(emptyState);
      expect(conversation.activeTarget.value).toBeNull();
      expect(messaging.timelineEl.value).toBeNull();
      expect(messaging.timelineEdgeScroller.value).toBeNull();
      expect(dmMessaging.timelineEl.value).toBeNull();
      expect(dmMessaging.timelineEdgeScroller.value).toBeNull();

      conversation.activeDraft.value = "unselected edit";
      conversation.activeForumTitle.value = "unselected title";
      expect(messaging.draft.value).toBe("room-draft");
      expect(messaging.forumPostTitle.value).toBe("room-title");
      expect(dmMessaging.draft.value).toBe("dm-draft");
      currentChannel.value = null;
      expect(displayedState(conversation)).toEqual(emptyState);

      isApplyingRoute.value = true;
      expect(conversation.contentAreaIsLoadingMessages.value).toBe(true);
    } finally {
      finishHistory({ messages: [], firstArchiveId: null, complete: true });
      await pendingLoad;
      scope.stop();
    }
  });

  test.each(["channel", "group", "dm"])("a selected %s keeps its own state and draft writes", async (surface) => {
    const ui = useChatShellState();
    ui.activePage.value = "chat";
    ui.sidebarMode.value = surface === "channel" ? "channels" : "dms";
    const messaging = messageState("room");
    const dmMessaging = messageState("dm");
    const currentChannel = ref({ id: "general", name: "General", isGroupDm: surface === "group" });
    const activePeerJid = ref(surface === "dm" ? "bob@example.com" : null);
    const scope = effectScope();
    const conversation = scope.run(() => useActiveConversation({
      ui, waddles: { currentChannel, isLoadingStructure: ref(false) }, messaging, dmMessaging,
      dmConversations: { activePeerJid, conversations: ref([{ peerJid: "bob@example.com", peerUsername: "bob" }]) },
      isApplyingRoute: ref(false),
    } as unknown as Parameters<typeof useActiveConversation>[0]))!;
    try {
      const selected = surface === "dm" ? dmMessaging : messaging;
      expect(conversation.activeTarget.value).toBe(selected);
      expect(displayedState(conversation)).toEqual({
        messages: selected.messages.value, firstUnseenId: selected.firstUnseenId.value,
        draft: selected.draft.value, forumTitle: surface === "dm" ? "" : messaging.forumPostTitle.value,
        typingUsers: selected.typingUsers.value,
        loading: true, loadingOlder: true, hasOlder: true, sending: true,
        searchResults: selected.searchResults.value, searching: true, upload: selected.uploadProgress.value,
      });
      conversation.activeDraft.value = "new draft";
      expect(selected.draft.value).toBe("new draft");
      conversation.activeForumTitle.value = "new title";
      expect(messaging.forumPostTitle.value).toBe(surface === "dm" ? "room-title" : "new title");

      conversation.setContentAreaRef({ messagesContainer: {}, scrollToPinnedEdge: async () => true } as unknown as ContentAreaHandle);
      await nextTick();
      expect(selected.timelineEl.value).not.toBeNull();
      expect(selected.timelineEdgeScroller.value).not.toBeNull();
      const inactive = surface === "dm" ? messaging : dmMessaging;
      expect(inactive.timelineEl.value).toBeNull();

      ui.sidebarMode.value = "dms";
      activePeerJid.value = null;
      currentChannel.value.isGroupDm = false;
      await nextTick();
      expect(conversation.activeFirstUnseenId.value).toBeNull();
      expect(conversation.activeIsLoadingOlderMessages.value).toBe(false);
      expect(selected.timelineEl.value).toBeNull();
      expect(selected.timelineEdgeScroller.value).toBeNull();
    } finally {
      scope.stop();
    }
  });

  test("DM mode rejects an old channel but allows group and direct chats", () => {
    const ui = useChatShellState();
    ui.sidebarMode.value = "dms";
    const currentChannel = ref<ChannelSummary | null>({ id: "general", name: "General" });
    const activePeerJid = ref<string | null>(null);
    const roomMessages = [{ id: "room-message" }];
    const directMessages = [{ id: "direct-message" }];
    const scope = effectScope();
    const conversation = scope.run(() => useActiveConversation({
      ui,
      waddles: { currentChannel, isLoadingStructure: ref(false) },
      messaging: {
        messages: ref(roomMessages), typingUsers: ref(["room-occupant"]),
        timelineEl: ref(null), timelineEdgeScroller: ref(null),
      },
      dmMessaging: {
        messages: ref(directMessages), typingUsers: ref(["bob"]),
        timelineEl: ref(null), timelineEdgeScroller: ref(null),
      },
      dmConversations: { activePeerJid },
      isApplyingRoute: ref(false),
    } as unknown as Parameters<typeof useActiveConversation>[0]))!;
    try {
      expect(conversation.activeRoomChannel.value).toBeNull();
      expect(conversation.activeMessages.value).toEqual([]);
      expect(conversation.activeTypingUsers.value).toEqual([]);

      currentChannel.value = { id: "friends", name: "Friends", isGroupDm: true };
      expect(conversation.activeRoomChannel.value?.id).toBe("friends");
      expect(conversation.activeMessages.value).toEqual(roomMessages);

      activePeerJid.value = "bob@example.com";
      expect(conversation.activeRoomChannel.value).toBeNull();
      expect(conversation.activeMessages.value).toEqual(directMessages);

      activePeerJid.value = null;
      ui.sidebarMode.value = "channels";
      currentChannel.value = { id: "general", name: "General" };
      expect(conversation.activeRoomChannel.value?.id).toBe("general");
      expect(conversation.activeMessages.value).toEqual(roomMessages);
    } finally {
      scope.stop();
    }
  });
});
