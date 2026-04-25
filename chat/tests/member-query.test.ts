import { describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { XmppErrorEvent } from "../src/lib/xmpp-client";

type TestXmpp = {
  getRoomMembers?: (
    room: string,
    opts: { affiliation: "owner" | "admin" | "member" | "outcast" },
  ) => Promise<{ muc?: { users?: Array<{ jid?: string; affiliation?: string }> } }>;
  getItems?: (jid: string, node: string) => Promise<{ items?: Array<{ content?: unknown }> }>;
  getAvatar?: (jid: string, id: string) => Promise<{ content?: unknown }>;
  getVCard?: (jid: string) => Promise<{ records?: unknown[] }>;
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
  (client as unknown as { connect: () => Promise<void>; xmpp: TestXmpp; connected: boolean }).connect = mock(async () => {});
  (client as unknown as { connect: () => Promise<void>; xmpp: TestXmpp; connected: boolean }).xmpp = xmpp;
  (client as unknown as { connect: () => Promise<void>; xmpp: TestXmpp; connected: boolean }).connected = true;
  return client;
}

describe("BrowserXmppClient.listRoomMembers", () => {
  test("fails visibly when stanza member-query support is missing", async () => {
    const client = clientWithXmpp({});
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));

    await expect(client.listRoomMembers("general")).rejects.toThrow("missing getRoomMembers");

    expect(errors).toHaveLength(1);
    expect(errors[0]).toMatchObject({
      kind: "member-query",
      recoverable: false,
    });
  });

  test("queries affiliations independently and returns successful members", async () => {
    const getRoomMembers = mock(async (
      room: string,
      opts: { affiliation: "owner" | "admin" | "member" | "outcast" },
    ) => {
      if (opts.affiliation === "admin") {
        throw { condition: "forbidden" };
      }
      if (opts.affiliation === "outcast") {
        throw { error: { condition: "service-unavailable" } };
      }
      if (opts.affiliation === "member") {
        return { muc: { users: [{ jid: "bob@example.com" }] } };
      }
      return { muc: { users: [] } };
    });
    const client = clientWithXmpp({ getRoomMembers });
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));

    const members = await client.listRoomMembers("general", { roomJid: "room-123@conference.example.net" });

    expect(members).toEqual([{
      jid: "bob@example.com",
      username: "bob",
      avatar_url: null,
      role: "member",
      joined_at: "",
    }]);
    expect(getRoomMembers.mock.calls.map((call) => call[0])).toEqual([
      "room-123@conference.example.net",
      "room-123@conference.example.net",
      "room-123@conference.example.net",
      "room-123@conference.example.net",
    ]);
    expect(errors.map((event) => event.condition)).toEqual(["forbidden", "service-unavailable"]);
    expect(errors[0].detail).toContain("forbidden affiliation query");
    expect(errors[1].detail).toContain("unsupported member query");
  });

  test("does not silently show zero members when only failed queries could contain members", async () => {
    const getRoomMembers = mock(async (
      _room: string,
      opts: { affiliation: "owner" | "admin" | "member" | "outcast" },
    ) => {
      if (opts.affiliation === "member") {
        throw { condition: "item-not-found" };
      }
      return { muc: { users: [] } };
    });
    const client = clientWithXmpp({ getRoomMembers });
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));

    await expect(client.listRoomMembers("general")).rejects.toThrow("refusing to show Members 0");

    expect(getRoomMembers).toHaveBeenCalledTimes(4);
    expect(errors.some((event) => event.detail.includes("reconstructed room JID may not match"))).toBe(true);
  });
});

describe("BrowserXmppClient.fetchUserAvatar", () => {
  test("fetches XEP-0084 avatar data before vCard fallback", async () => {
    const getItems = mock(async () => ({
      items: [{ content: { versions: [{ id: "hash1", mediaType: "image/png" }] } }],
    }));
    const getAvatar = mock(async () => ({
      content: { data: Buffer.from("avatar-bytes") },
    }));
    const getVCard = mock(async () => ({ records: [] }));
    const client = clientWithXmpp({ getItems, getAvatar, getVCard });

    await expect(client.fetchUserAvatar("bob@example.com/mobile")).resolves.toBe(
      `data:image/png;base64,${Buffer.from("avatar-bytes").toString("base64")}`,
    );
    expect(getItems).toHaveBeenCalledWith("bob@example.com", "urn:xmpp:avatar:metadata");
    expect(getAvatar).toHaveBeenCalledWith("bob@example.com", "hash1");
    expect(getVCard).not.toHaveBeenCalled();
  });

  test("falls back to vCard photo when PEP avatar is unavailable", async () => {
    const getItems = mock(async () => {
      throw { condition: "item-not-found" };
    });
    const getVCard = mock(async () => ({
      records: [{ type: "photo", mediaType: "image/jpeg", data: Buffer.from("vcard-photo") }],
    }));
    const client = clientWithXmpp({ getItems, getVCard });

    await expect(client.fetchUserAvatar("carol@example.com")).resolves.toBe(
      `data:image/jpeg;base64,${Buffer.from("vcard-photo").toString("base64")}`,
    );
    expect(getVCard).toHaveBeenCalledWith("carol@example.com");
  });

  test("falls back to vCard external photo URLs", async () => {
    const getItems = mock(async () => ({ items: [] }));
    const getVCard = mock(async () => ({
      records: [{ type: "photo", url: "https://avatars.example.com/dana.png" }],
    }));
    const client = clientWithXmpp({ getItems, getVCard });

    await expect(client.fetchUserAvatar("dana@example.com")).resolves.toBe("https://avatars.example.com/dana.png");
  });

  test("returns null when avatar sources are unsupported or empty", async () => {
    const getItems = mock(async () => ({ items: [] }));
    const getVCard = mock(async () => ({ records: [] }));
    const client = clientWithXmpp({ getItems, getVCard });

    await expect(client.fetchUserAvatar("dana@example.com")).resolves.toBeNull();
  });
});
