import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import { useWaddleDirectory } from "../src/waddles/directory";

const SESSION: WaddleSession = {
  session_id: "session-1",
  username: "alice",
  avatar_url: null,
  xmpp_localpart: "alice",
  jid: "alice@example.com",
  xmpp_websocket_url: "wss://example.com/ws",
  is_expired: false,
  expires_at: null,
};

describe("useWaddleDirectory community provisioning", () => {
  test("routes every mutation through the typed community facade", async () => {
    const configureMucRoom = mock(async () => undefined);
    const createMucRoom = mock(async () => ({
      roomJid: "general@muc.example.com",
    }));
    const createSpaceNode = mock(async () => ({
      node: "engineering",
      serviceJid: "spaces.example.com",
    }));
    const createMucInSpace = mock(async () => ({
      roomJid: "platform@muc.example.com",
      spaceNode: "engineering",
      spacesServiceJid: "spaces.example.com",
    }));
    const createSpaceWithMuc = mock(async () => ({
      roomJid: "design@muc.example.com",
      spaceNode: "design-team",
      spacesServiceJid: "spaces.example.com",
    }));
    const moveMucToSpace = mock(async () => undefined);
    const client = {
      communityProvisioning: {
        configureMucRoom,
        createMucRoom,
        createSpaceNode,
        createMucInSpace,
        createSpaceWithMuc,
        moveMucToSpace,
      },
      discoverTopology: mock(async () => ({
        spaces: [],
        rooms: [],
      })),
      listRoomMembers: mock(async () => []),
    } as unknown as BrowserXmppClient;
    const actionError = ref("");
    const directory = useWaddleDirectory(
      ref(client),
      ref(SESSION),
      (error) => error instanceof Error ? error.message : String(error),
      actionError,
      () => {
        actionError.value = "";
      },
    );

    directory.createChannelForm.value = {
      intent: "muc",
      name: "General",
      description: "Default room",
      muc_type: "text",
    };
    expect(await directory.createChannel()).toEqual({
      intent: "muc",
      channelId: "general",
      channelJid: "general@muc.example.com",
      channelName: "General",
      channelType: "text",
    });
    expect(createMucRoom).toHaveBeenCalledWith({
      mucServiceJid: "muc.example.com",
      roomLocalpart: "general",
      name: "General",
      description: "Default room",
      mucType: "text",
    });

    directory.createChannelForm.value = {
      intent: "space",
      name: "Engineering",
      description: "Product engineering",
    };
    expect(await directory.createChannel()).toEqual({
      intent: "space",
      spaceId: "engineering",
      spaceName: "Engineering",
    });
    expect(createSpaceNode).toHaveBeenCalledWith({
      spacesServiceJid: "spaces.example.com",
      name: "Engineering",
      description: "Product engineering",
    });

    directory.createChannelForm.value = {
      intent: "space-muc",
      space_node: "engineering",
      name: "Platform",
      description: "Platform team",
      muc_type: "forum",
    };
    expect(await directory.createChannel()).toEqual({
      intent: "space-muc",
      channelId: "platform",
      channelJid: "platform@muc.example.com",
      channelName: "Platform",
      channelType: "forum",
      spaceNode: "engineering",
    });
    expect(createMucInSpace).toHaveBeenCalledWith({
      mucServiceJid: "muc.example.com",
      spacesServiceJid: "spaces.example.com",
      roomLocalpart: "platform",
      name: "Platform",
      description: "Platform team",
      mucType: "forum",
      spaceNode: "engineering",
    });

    directory.createChannelForm.value = {
      intent: "space-with-muc",
      space_name: "Design Team",
      space_description: "Product design",
      muc_name: "Design",
      muc_description: "Design room",
      muc_type: "text",
    };
    expect(await directory.createChannel()).toEqual({
      intent: "space-with-muc",
      channelId: "design",
      channelJid: "design@muc.example.com",
      channelName: "Design",
      channelType: "text",
      spaceNode: "design-team",
    });
    expect(createSpaceWithMuc).toHaveBeenCalledWith({
      mucServiceJid: "muc.example.com",
      spacesServiceJid: "spaces.example.com",
      spaceName: "Design Team",
      spaceDescription: "Product design",
      roomLocalpart: "design",
      mucName: "Design",
      mucDescription: "Design room",
      mucType: "text",
    });

    directory.channels.value = [{
      id: "general",
      jid: "general@muc.example.com",
      name: "General",
      description: "Default room",
      channel_type: "text",
      spaceId: "engineering",
    }];
    directory.activeChannelId.value = "general";
    await Promise.resolve();
    directory.editChannelForm.value = {
      name: "General Help",
      description: "Help room",
      position: 0,
      pinPermission: "admins",
    };
    expect(await directory.updateChannel()).toBe(true);
    expect(configureMucRoom).toHaveBeenCalledWith({
      roomJid: "general@muc.example.com",
      name: "General Help",
      description: "Help room",
      pinPermission: "admins",
    });

    expect(await directory.moveChannelToSpace("general", "design-team")).toBe(true);
    expect(moveMucToSpace).toHaveBeenCalledWith({
      spacesServiceJid: "spaces.example.com",
      targetSpaceNode: "design-team",
      mucJid: "general@muc.example.com",
      name: "General Help",
      autojoin: true,
    });
    expect(actionError.value).toBe("");
  });
});
