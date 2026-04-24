import { describe, it, expect, mock, beforeEach } from "bun:test";
import {
  createMucRoom,
  createSpaceNode,
  publishMucToSpace,
  createMucInSpace,
  createSpaceWithMuc,
  NS_BOOKMARKS_1,
} from "@/lib/xmpp/protocol-helpers";

// ── Mock factory ──────────────────────────────────────────────────────────

function makeXmppMock() {
  const handlers: Map<string, Array<(...args: unknown[]) => void>> = new Map();

  const on = mock((event: string, handler: (...args: unknown[]) => void) => {
    if (!handlers.has(event)) handlers.set(event, []);
    handlers.get(event)!.push(handler);
  });
  const off = mock((event: string, handler: (...args: unknown[]) => void) => {
    const list = handlers.get(event) ?? [];
    const idx = list.indexOf(handler);
    if (idx !== -1) list.splice(idx, 1);
  });
  const emit = (event: string, payload: unknown) => {
    for (const h of handlers.get(event) ?? []) h(payload);
  };

  const joinRoom = mock((_room: string, _nick: string, _opts?: unknown) => {
    // Simulate self-presence arriving synchronously after join
    Promise.resolve().then(() => {
      const [room, nick] = [_room as string, _nick as string];
      emit("muc:available", { from: `${room}/${nick}`, muc: { statusCodes: ["201"] } });
    });
    return Promise.resolve();
  });

  const leaveRoom = mock(() => Promise.resolve());
  const sendIQ = mock(() => Promise.resolve({}));

  return { on, off, emit, joinRoom, leaveRoom, sendIQ, handlers };
}

// ── XEP-0045: createMucRoom ───────────────────────────────────────────────

describe("createMucRoom", () => {
  it("joins the room then sends an owner configuration IQ", async () => {
    const xmpp = makeXmppMock();

    const result = await createMucRoom(xmpp as any, "muc.example.org", {
      roomLocalpart: "general",
      nick: "alice",
      name: "General",
      mucType: "text",
    });

    expect(result.roomJid).toBe("general@muc.example.org");

    // Must join before configuring
    expect(xmpp.joinRoom).toHaveBeenCalledTimes(1);
    expect(xmpp.joinRoom).toHaveBeenCalledWith(
      "general@muc.example.org",
      "alice",
      expect.objectContaining({ history: { maxStanzas: 0 } }),
    );

    // Must send owner config IQ
    expect(xmpp.sendIQ).toHaveBeenCalledTimes(1);
    expect(xmpp.sendIQ).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "set",
        to: "general@muc.example.org",
        muc: expect.objectContaining({
          type: "configure",
          form: expect.objectContaining({
            type: "submit",
            fields: expect.arrayContaining([
              expect.objectContaining({ name: "muc#roomconfig_roomname", value: "General" }),
            ]),
          }),
        }),
      }),
    );

    // Must leave after config so BrowserXmppClient can re-join via switchRoom
    expect(xmpp.leaveRoom).toHaveBeenCalledTimes(1);
    expect(xmpp.leaveRoom).toHaveBeenCalledWith("general@muc.example.org", "alice");
  });

  it("includes FORM_TYPE field in the roomconfig form", async () => {
    const xmpp = makeXmppMock();

    await createMucRoom(xmpp as any, "muc.example.org", {
      roomLocalpart: "eng",
      nick: "alice",
      name: "Engineering",
    });

    const [iq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    const fields: Array<{ name: string; value: string }> = iq.muc.form.fields;
    const formType = fields.find((f) => f.name === "FORM_TYPE");
    expect(formType?.value).toBe("http://jabber.org/protocol/muc#roomconfig");
  });

  it("sets forum config field when mucType is forum", async () => {
    const xmpp = makeXmppMock();

    await createMucRoom(xmpp as any, "muc.example.org", {
      roomLocalpart: "ideas",
      nick: "bob",
      name: "Ideas",
      mucType: "forum",
    });

    const [iq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    const fields: Array<{ name: string; value: string }> = iq.muc.form.fields;
    const forumField = fields.find((f) => f.name === "muc#roomconfig_forum");
    expect(forumField?.value).toBe("1");
  });

  it("does not set forum config field when mucType is text", async () => {
    const xmpp = makeXmppMock();

    await createMucRoom(xmpp as any, "muc.example.org", {
      roomLocalpart: "chat",
      nick: "bob",
      name: "Chat",
      mucType: "text",
    });

    const [iq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    const fields: Array<{ name: string; value: string }> = iq.muc.form.fields;
    const forumField = fields.find((f) => f.name === "muc#roomconfig_forum");
    expect(forumField).toBeUndefined();
  });

  it("includes description field when provided", async () => {
    const xmpp = makeXmppMock();

    await createMucRoom(xmpp as any, "muc.example.org", {
      roomLocalpart: "general",
      nick: "alice",
      name: "General",
      description: "The general channel",
    });

    const [iq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    const fields: Array<{ name: string; value: string }> = iq.muc.form.fields;
    const desc = fields.find((f) => f.name === "muc#roomconfig_roomdesc");
    expect(desc?.value).toBe("The general channel");
  });

  it("propagates sendIQ errors", async () => {
    const xmpp = makeXmppMock();
    xmpp.sendIQ.mockRejectedValueOnce({ error: { condition: "conflict", text: "Room already exists" } });

    await expect(
      createMucRoom(xmpp as any, "muc.example.org", {
        roomLocalpart: "existing",
        nick: "alice",
        name: "Existing",
      }),
    ).rejects.toMatchObject({ error: { condition: "conflict" } });
  });

  it("propagates joinRoom errors", async () => {
    const xmpp = makeXmppMock();
    xmpp.joinRoom.mockRejectedValueOnce(new Error("forbidden"));

    await expect(
      createMucRoom(xmpp as any, "muc.example.org", {
        roomLocalpart: "forbidden",
        nick: "alice",
        name: "Forbidden",
      }),
    ).rejects.toThrow("forbidden");
  });

  it("does not use any waddle:* command node", async () => {
    const xmpp = makeXmppMock();

    await createMucRoom(xmpp as any, "muc.example.org", {
      roomLocalpart: "test",
      nick: "alice",
      name: "Test",
    });

    const calls = xmpp.sendIQ.mock.calls as [unknown][][];
    for (const [iq] of calls) {
      const node = (iq as any)?.command?.node ?? "";
      expect(node).not.toMatch(/^waddle:/);
    }
  });
});

// ── XEP-0060: createSpaceNode ─────────────────────────────────────────────

describe("createSpaceNode", () => {
  it("sends a pubsub create+configure IQ with spaces:0 type", async () => {
    const xmpp = makeXmppMock();

    const result = await createSpaceNode(xmpp as any, "spaces.example.org", {
      nodeId: "my-space",
      name: "My Space",
      description: "A test space",
    });

    expect(result.node).toBe("my-space");
    expect(result.serviceJid).toBe("spaces.example.org");

    expect(xmpp.sendIQ).toHaveBeenCalledTimes(1);
    expect(xmpp.sendIQ).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "set",
        to: "spaces.example.org",
        pubsub: expect.objectContaining({
          create: { node: "my-space" },
          configure: expect.objectContaining({
            node: "my-space",
            form: expect.objectContaining({
              type: "submit",
            }),
          }),
        }),
      }),
    );
  });

  it("includes pubsub#type = urn:xmpp:spaces:0 in config form", async () => {
    const xmpp = makeXmppMock();

    await createSpaceNode(xmpp as any, "spaces.example.org", {
      nodeId: "eng",
      name: "Engineering",
    });

    const [iq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    const fields: Array<{ name: string; value: string }> = iq.pubsub.configure.form.fields;
    const typeField = fields.find((f) => f.name === "pubsub#type");
    expect(typeField?.value).toBe("urn:xmpp:spaces:0");
  });

  it("includes pubsub#title from the name param", async () => {
    const xmpp = makeXmppMock();

    await createSpaceNode(xmpp as any, "spaces.example.org", {
      nodeId: "eng",
      name: "Engineering",
    });

    const [iq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    const fields: Array<{ name: string; value: string }> = iq.pubsub.configure.form.fields;
    const titleField = fields.find((f) => f.name === "pubsub#title");
    expect(titleField?.value).toBe("Engineering");
  });

  it("auto-generates a node id when none is provided", async () => {
    const xmpp = makeXmppMock();

    const result = await createSpaceNode(xmpp as any, "spaces.example.org", {
      name: "Auto Space",
    });

    expect(typeof result.node).toBe("string");
    expect(result.node.length).toBeGreaterThan(0);
  });

  it("propagates sendIQ errors", async () => {
    const xmpp = makeXmppMock();
    xmpp.sendIQ.mockRejectedValueOnce({ error: { condition: "conflict" } });

    await expect(
      createSpaceNode(xmpp as any, "spaces.example.org", { name: "Dup" }),
    ).rejects.toMatchObject({ error: { condition: "conflict" } });
  });
});

// ── XEP-0402/XEP-0503: publishMucToSpace ─────────────────────────────────

describe("publishMucToSpace", () => {
  it("sends a pubsub publish IQ with bookmarks:1 item", async () => {
    const xmpp = makeXmppMock();

    await publishMucToSpace(
      xmpp as any,
      "spaces.example.org",
      "my-space",
      "general@muc.example.org",
      { name: "General", autojoin: true },
    );

    expect(xmpp.sendIQ).toHaveBeenCalledTimes(1);
    expect(xmpp.sendIQ).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "set",
        to: "spaces.example.org",
        pubsub: expect.objectContaining({
          publish: expect.objectContaining({
            node: "my-space",
            item: expect.objectContaining({
              id: "general@muc.example.org",
              content: expect.objectContaining({
                itemType: NS_BOOKMARKS_1,
                name: "General",
                autojoin: true,
              }),
            }),
          }),
        }),
      }),
    );
  });

  it("defaults autojoin to true when not specified", async () => {
    const xmpp = makeXmppMock();

    await publishMucToSpace(
      xmpp as any,
      "spaces.example.org",
      "my-space",
      "general@muc.example.org",
      { name: "General" },
    );

    const [iq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    expect(iq.pubsub.publish.item.content.autojoin).toBe(true);
  });

  it("propagates sendIQ errors", async () => {
    const xmpp = makeXmppMock();
    xmpp.sendIQ.mockRejectedValueOnce({ error: { condition: "forbidden" } });

    await expect(
      publishMucToSpace(xmpp as any, "spaces.example.org", "sp", "r@muc.x", { name: "R" }),
    ).rejects.toMatchObject({ error: { condition: "forbidden" } });
  });
});

// ── Composed: createMucInSpace ────────────────────────────────────────────

describe("createMucInSpace", () => {
  it("creates the MUC then publishes a bookmark into the existing space", async () => {
    const xmpp = makeXmppMock();

    const result = await createMucInSpace(
      xmpp as any,
      "muc.example.org",
      "spaces.example.org",
      {
        roomLocalpart: "dev",
        nick: "alice",
        name: "Dev",
        mucType: "text",
        spaceNode: "engineering",
      },
    );

    expect(result.roomJid).toBe("dev@muc.example.org");
    expect(result.spaceNode).toBe("engineering");
    expect(result.spacesServiceJid).toBe("spaces.example.org");

    // sendIQ called twice: MUC config + bookmark publish
    expect(xmpp.sendIQ).toHaveBeenCalledTimes(2);

    // First IQ: MUC configure
    const [mucIq] = (xmpp.sendIQ.mock.calls[0] ?? []) as [any];
    expect(mucIq.muc?.type).toBe("configure");

    // Second IQ: pubsub publish
    const [pubsubIq] = (xmpp.sendIQ.mock.calls[1] ?? []) as [any];
    expect(pubsubIq.pubsub?.publish?.node).toBe("engineering");
    expect(pubsubIq.pubsub?.publish?.item?.id).toBe("dev@muc.example.org");
  });

  it("does not call createSpaceNode (uses existing space)", async () => {
    const xmpp = makeXmppMock();

    await createMucInSpace(
      xmpp as any,
      "muc.example.org",
      "spaces.example.org",
      {
        roomLocalpart: "dev",
        nick: "alice",
        name: "Dev",
        spaceNode: "engineering",
      },
    );

    // Only 2 sendIQ calls: muc config + bookmark; no pubsub create
    expect(xmpp.sendIQ).toHaveBeenCalledTimes(2);
    for (const [iq] of xmpp.sendIQ.mock.calls as [any][][]) {
      expect(iq.pubsub?.create).toBeUndefined();
    }
  });

  it("propagates MUC creation error without publishing bookmark", async () => {
    const xmpp = makeXmppMock();
    // joinRoom will reject
    xmpp.joinRoom.mockRejectedValueOnce(new Error("forbidden"));

    await expect(
      createMucInSpace(
        xmpp as any,
        "muc.example.org",
        "spaces.example.org",
        {
          roomLocalpart: "dev",
          nick: "alice",
          name: "Dev",
          spaceNode: "engineering",
        },
      ),
    ).rejects.toThrow("forbidden");

    // Bookmark publish must not have been called
    const pubsubCalls = (xmpp.sendIQ.mock.calls as [any][][]).filter(
      ([iq]) => iq.pubsub?.publish,
    );
    expect(pubsubCalls).toHaveLength(0);
  });
});

// ── Composed: createSpaceWithMuc ──────────────────────────────────────────

describe("createSpaceWithMuc", () => {
  it("creates the space then MUC then publishes bookmark — in order", async () => {
    const xmpp = makeXmppMock();
    const callOrder: string[] = [];

    xmpp.sendIQ.mockImplementation((iq: any) => {
      if (iq.pubsub?.create) callOrder.push("space-create");
      else if (iq.muc?.type === "configure") callOrder.push("muc-configure");
      else if (iq.pubsub?.publish) callOrder.push("bookmark-publish");
      return Promise.resolve({});
    });

    const result = await createSpaceWithMuc(
      xmpp as any,
      "muc.example.org",
      "spaces.example.org",
      {
        spaceNodeId: "eng",
        spaceName: "Engineering",
        roomLocalpart: "general",
        nick: "alice",
        mucName: "General",
        mucType: "text",
      },
    );

    expect(result.roomJid).toBe("general@muc.example.org");
    expect(result.spaceNode).toBe("eng");
    expect(result.spacesServiceJid).toBe("spaces.example.org");

    // Strict ordering
    expect(callOrder).toEqual(["space-create", "muc-configure", "bookmark-publish"]);

    // Total: 3 sendIQ calls
    expect(xmpp.sendIQ).toHaveBeenCalledTimes(3);
  });

  it("propagates space creation error; MUC creation and bookmark not attempted", async () => {
    const xmpp = makeXmppMock();
    xmpp.sendIQ.mockRejectedValueOnce({ error: { condition: "not-allowed" } });

    await expect(
      createSpaceWithMuc(
        xmpp as any,
        "muc.example.org",
        "spaces.example.org",
        {
          spaceName: "Eng",
          roomLocalpart: "general",
          nick: "alice",
          mucName: "General",
        },
      ),
    ).rejects.toMatchObject({ error: { condition: "not-allowed" } });

    // Only the failed space-create IQ was attempted
    expect(xmpp.sendIQ).toHaveBeenCalledTimes(1);
    expect(xmpp.joinRoom).not.toHaveBeenCalled();
  });

  it("propagates MUC creation error; bookmark not published", async () => {
    const xmpp = makeXmppMock();
    // First sendIQ (space create) succeeds; joinRoom fails
    xmpp.joinRoom.mockRejectedValueOnce(new Error("conflict"));

    await expect(
      createSpaceWithMuc(
        xmpp as any,
        "muc.example.org",
        "spaces.example.org",
        {
          spaceName: "Eng",
          roomLocalpart: "general",
          nick: "alice",
          mucName: "General",
        },
      ),
    ).rejects.toThrow("conflict");

    // Space create IQ was sent; bookmark publish was not
    const pubsubCalls = (xmpp.sendIQ.mock.calls as [any][][]).filter(
      ([iq]) => iq.pubsub?.publish,
    );
    expect(pubsubCalls).toHaveLength(0);
  });

  it("does not use any waddle:* command node", async () => {
    const xmpp = makeXmppMock();

    await createSpaceWithMuc(
      xmpp as any,
      "muc.example.org",
      "spaces.example.org",
      {
        spaceName: "Eng",
        roomLocalpart: "general",
        nick: "alice",
        mucName: "General",
      },
    );

    for (const [iq] of xmpp.sendIQ.mock.calls as [any][][]) {
      const node = iq?.command?.node ?? "";
      expect(node).not.toMatch(/^waddle:/);
    }
  });
});
