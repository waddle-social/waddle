import { describe, expect, test } from "bun:test";
import { effectScope, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useActiveConversation } from "../src/shell/controllers/use-active-conversation";
import type { ChannelSummary } from "../src/lib/chat-types";

describe("active conversation surface", () => {
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
