import { describe, expect, mock, test } from "bun:test";
import { CommunityProvisioning } from "../src/lib/xmpp/client-community-provisioning";
import { withFakeDomParser } from "./helpers/disco-xml";

const OWNER_CONFIG = [
  '<iq type="result">',
  '<query xmlns="http://jabber.org/protocol/muc#owner">',
  '<x xmlns="jabber:x:data" type="form">',
  '<field var="FORM_TYPE"><value>http://jabber.org/protocol/muc#roomconfig</value></field>',
  '<field var="muc#roomconfig_roomname"><value>Old name</value></field>',
  "</x>",
  "</query>",
  "</iq>",
].join("");

describe("CommunityProvisioning", () => {
  test("acquires one connected transport per operation and injects the live nick", async () => {
    const sendRawIq = mock(async (xml: string) =>
      xml.includes('type="get"') ? OWNER_CONFIG : '<iq type="result"/>'
    );
    const joinRoom = mock(async () => undefined);
    const leaveRoom = mock(async () => undefined);
    const requireConnectedXmpp = mock(async () => ({
      send_raw_iq: sendRawIq,
      join_room: joinRoom,
      leave_room: leaveRoom,
    }));
    let nick = "alice";
    const provisioning = new CommunityProvisioning({
      requireConnectedXmpp,
      nick: () => nick,
    });

    await withFakeDomParser(async () => {
      await provisioning.configureMucRoom({
        roomJid: "general@rooms.example.com",
        name: "General",
        description: "Default room",
        pinPermission: "admins",
      });
    });

    expect(await provisioning.createMucRoom({
      mucServiceJid: "rooms.example.com",
      roomLocalpart: "general",
      name: "General",
      mucType: "text",
    })).toEqual({ roomJid: "general@rooms.example.com" });

    expect(await provisioning.createSpaceNode({
      spacesServiceJid: "spaces.example.com",
      nodeId: "engineering",
      name: "Engineering",
    })).toEqual({
      node: "engineering",
      serviceJid: "spaces.example.com",
    });

    nick = "alice-mobile";
    expect(await provisioning.createMucInSpace({
      mucServiceJid: "rooms.example.com",
      spacesServiceJid: "spaces.example.com",
      roomLocalpart: "platform",
      name: "Platform",
      mucType: "forum",
      spaceNode: "engineering",
    })).toEqual({
      roomJid: "platform@rooms.example.com",
      spaceNode: "engineering",
      spacesServiceJid: "spaces.example.com",
    });

    nick = "alice-desktop";
    expect(await provisioning.createSpaceWithMuc({
      mucServiceJid: "rooms.example.com",
      spacesServiceJid: "spaces.example.com",
      spaceName: "Design Team",
      roomLocalpart: "design",
      mucName: "Design",
      mucType: "text",
    })).toEqual({
      roomJid: "design@rooms.example.com",
      spaceNode: "design-team",
      spacesServiceJid: "spaces.example.com",
    });

    await provisioning.moveMucToSpace({
      spacesServiceJid: "spaces.example.com",
      targetSpaceNode: "design-team",
      mucJid: "platform@rooms.example.com",
      name: "Platform",
      autojoin: true,
    });

    expect(requireConnectedXmpp).toHaveBeenCalledTimes(6);
    expect(joinRoom.mock.calls.map((call) => call[1])).toEqual([
      "alice",
      "alice-mobile",
      "alice-desktop",
    ]);
    expect(leaveRoom.mock.calls.map((call) => call[1])).toEqual([
      "alice",
      "alice-mobile",
      "alice-desktop",
    ]);
  });

  test("does not reacquire transport when a provisioning operation fails", async () => {
    const requireConnectedXmpp = mock(async () => ({
      send_raw_iq: mock(async () => {
        throw new Error("service unavailable");
      }),
    }));
    const provisioning = new CommunityProvisioning({
      requireConnectedXmpp,
      nick: () => "alice",
    });

    await expect(provisioning.moveMucToSpace({
      spacesServiceJid: "spaces.example.com",
      targetSpaceNode: "engineering",
      mucJid: "general@rooms.example.com",
      name: "General",
    })).rejects.toThrow("service unavailable");
    expect(requireConnectedXmpp).toHaveBeenCalledTimes(1);
  });
});
