import { describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient, RoomMemberListUnavailableError } from "../src/lib/xmpp-client";
import type { XmppErrorEvent } from "../src/lib/xmpp-client";

type TestXmpp = {
  list_room_members?: (
    room: string,
    affiliation: "owner" | "admin" | "member" | "outcast",
  ) => Promise<Array<{ jid?: string }>>;
  request_avatar?: (jid: string) => Promise<{ jid: string; id: string; mime_type: string; data?: Uint8Array; url?: string } | null>;
};

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

function clientWithXmpp(xmpp: TestXmpp) {
  const client = new BrowserXmppClient(session());
  const internals = client as unknown as {
    connect: () => Promise<void>;
    requireReadyRoomXmpp: (roomJid: string) => Promise<TestXmpp>;
    xmpp: TestXmpp;
    connected: boolean;
  };
  internals.connect = mock(async () => {});
  internals.requireReadyRoomXmpp = mock(async () => xmpp);
  internals.xmpp = xmpp;
  internals.connected = true;
  return client;
}

describe("BrowserXmppClient.listRoomMembers", () => {
  test("fails visibly when Rust member-query support is missing", async () => {
    const client = clientWithXmpp({});
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));

    await expect(client.listRoomMembers("general")).rejects.toThrow("missing list_room_members");

    expect(errors).toHaveLength(1);
    expect(errors[0]).toMatchObject({
      kind: "member-query",
      recoverable: false,
    });
  });

  test("queries affiliations independently and returns successful members", async () => {
    const listRoomMembers = mock(async (
      room: string,
      affiliation: "owner" | "admin" | "member" | "outcast",
    ) => {
      if (affiliation === "admin") {
        throw { condition: "forbidden" };
      }
      if (affiliation === "outcast") {
        throw { error: { condition: "service-unavailable" } };
      }
      if (affiliation === "member") {
        return [{ jid: "bob@example.com" }];
      }
      return [];
    });
    const client = clientWithXmpp({ list_room_members: listRoomMembers });
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));

    const members = await client.listRoomMembers("general", { roomJid: "room-123@conference.example.net" });

    expect(members).toEqual([{
      jid: "bob@example.com",
      username: "bob",
      avatar_url: null,
      affiliation: "member",
      joined_at: "",
    }]);
    expect(listRoomMembers.mock.calls.map((call) => call[0])).toEqual([
      "room-123@conference.example.net",
      "room-123@conference.example.net",
      "room-123@conference.example.net",
      "room-123@conference.example.net",
    ]);
    expect(errors.map((event) => event.condition)).toEqual(["forbidden", "service-unavailable"]);
    expect(errors[0].detail).toContain("forbidden affiliation query");
    expect(errors[1].detail).toContain("unsupported member query");
  });

  test("reports unavailable member lists without the retired zero-member failure copy", async () => {
    const listRoomMembers = mock(async (
      _room: string,
      affiliation: "owner" | "admin" | "member" | "outcast",
    ) => {
      if (affiliation === "member") {
        throw { condition: "item-not-found" };
      }
      return [];
    });
    const client = clientWithXmpp({ list_room_members: listRoomMembers });
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));

    const result = client.listRoomMembers("general");

    await expect(result).rejects.toBeInstanceOf(RoomMemberListUnavailableError);
    await expect(result).rejects.toThrow("Member list is temporarily unavailable.");

    expect(listRoomMembers).toHaveBeenCalledTimes(4);
    expect(errors.some((event) => event.detail.includes("reconstructed room JID may not match"))).toBe(true);
  });
});

describe("BrowserXmppClient.fetchUserAvatar", () => {
  test("fetches avatar data through the Rust client with a bare JID", async () => {
    const requestAvatar = mock(async (jid: string) => ({
      jid,
      id: "hash1",
      mime_type: "image/png",
      data: new Uint8Array(Buffer.from("avatar-bytes")),
    }));
    const client = clientWithXmpp({ request_avatar: requestAvatar });

    await expect(client.fetchUserAvatar("bob@example.com/mobile")).resolves.toBe(
      `data:image/png;base64,${Buffer.from("avatar-bytes").toString("base64")}`,
    );
    expect(requestAvatar).toHaveBeenCalledWith("bob@example.com");
  });

  test("returns external avatar URLs from the Rust client", async () => {
    const requestAvatar = mock(async (jid: string) => {
      expect(jid).toBe("dana@example.com");
      return {
        jid,
        id: "vcard-extval",
        mime_type: "image/png",
        url: "https://avatars.example.com/dana.png",
      };
    });
    const client = clientWithXmpp({ request_avatar: requestAvatar });

    await expect(client.fetchUserAvatar("dana@example.com")).resolves.toBe("https://avatars.example.com/dana.png");
  });

  test("returns null when the Rust client has no avatar", async () => {
    const requestAvatar = mock(async () => null);
    const client = clientWithXmpp({ request_avatar: requestAvatar });

    await expect(client.fetchUserAvatar("dana@example.com")).resolves.toBeNull();
  });
});
