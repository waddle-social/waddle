import { describe, expect, test } from "bun:test";
import { ref } from "vue";
import { useChannelMessages } from "../src/channels/messages";
import { createNotifySettingsStore } from "../src/lib/notify-settings";
import type { WaddleSession } from "../src/lib/server-auth";
import type {
  BrowserXmppClient,
  RoomAccessChangedEvent,
} from "../src/lib/xmpp-client";
import { handlerStubs } from "./helpers/xmpp-client-mock";
import { renderVueComponent } from "./helpers/render-vue-sfc";

const roomJid = "private@muc.example.com";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

describe("channel access-required UI state", () => {
  test("tracks the active room's typed access state until the room becomes available", () => {
    let roomAccessHandler: ((event: RoomAccessChangedEvent) => void) | null = null;
    const client = {
      ...handlerStubs(),
      onRoomAccessChanged(handler: (event: RoomAccessChangedEvent) => void) {
        roomAccessHandler = handler;
        return () => {
          roomAccessHandler = null;
        };
      },
    } as unknown as BrowserXmppClient;
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(client),
      ref("space"),
      ref("private"),
      ref({
        id: "private",
        name: "Private",
        jid: roomJid,
        channel_type: "text",
      }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    roomAccessHandler?.({
      roomJid,
      state: "required",
      condition: "registration-required",
    });
    expect(messaging.currentRoomAccessRequirement.value).toEqual({
      roomJid,
      condition: "registration-required",
    });

    roomAccessHandler?.({
      roomJid,
      state: "available",
    });
    expect(messaging.currentRoomAccessRequirement.value).toBeNull();
  });

  test("hydrates a persisted access requirement when the client binds after reload", () => {
    const client = {
      ...handlerStubs(),
      listRoomAccessRequirements: () => [{
        roomJid,
        state: "required" as const,
        condition: "forbidden" as const,
      }],
    } as unknown as BrowserXmppClient;
    const messaging = useChannelMessages(
      ref(session()),
      ref(client),
      ref("space"),
      ref("private"),
      ref({
        id: "private",
        name: "Private",
        jid: roomJid,
        channel_type: "text",
      }),
      String,
      ref(""),
      () => {},
    );

    expect(messaging.currentRoomAccessRequirement.value).toEqual({
      roomJid,
      condition: "forbidden",
    });
  });

  test("renders an accessible channel-specific access explanation", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/TimelineEmptyState.vue",
      {
        variant: "access-required",
        isForumChannel: false,
        channelName: "Private",
      },
      import.meta.url,
    );

    expect(html).toContain('role="status"');
    expect(html).toContain("You need access to this channel");
    expect(html).toContain("Ask a space admin for access");
  });

  test("the channel surface replaces loading and composing with the access state", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/ContentArea.vue",
      {
        draft: "",
        forumTitle: "",
        pinnedPanelOpen: false,
        waddle: { id: "space", name: "Space" },
        channel: { id: "private", name: "Private", jid: roomJid, spaceId: "space" },
        roomJid,
        dmPeer: null,
        sidebarMode: "channels",
        messages: [],
        firstUnseenId: null,
        xmppStatus: { state: "online" },
        actionError: "Generic join failure",
        errorActionLabel: null,
        channelAccessRequired: true,
        updateAvailable: false,
        isApplyingUpdate: false,
        isLoadingMessages: true,
        isLoadingOlderMessages: false,
        hasOlderMessages: false,
        isSending: false,
        canManageChannels: false,
        memberCount: 0,
        memberState: "ready",
        typingUsers: [],
        currentUser: "alice",
        currentUserJid: "alice@example.com",
        selfFullJid: "alice@example.com/web",
        selfDomain: "example.com",
        avatarUrlByAuthor: {},
        authorJidByNick: {},
        mentionCandidates: [],
        roomHats: {},
        roomAuthority: {},
        roomPresence: {},
        roomLastSeen: {},
        slowModeCooldown: 0,
        searchResults: [],
        isSearching: false,
        uploadProgress: { uploading: false, progress: 0, filename: "" },
        threadIndex: new Map(),
        xmppClient: null,
        notifySettings: createNotifySettingsStore(),
        reactionMode: null,
      },
      import.meta.url,
    );

    expect(html).toContain("You need access to this channel");
    expect(html).not.toContain("Generic join failure");
    expect(html).not.toContain("message-list-skeleton");
    expect(html).not.toContain("<textarea");
  });
});
