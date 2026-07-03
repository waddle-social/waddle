/**
 * Unit tests for the presence module extracted from
 * `BrowserXmppClient` (`src/lib/xmpp/client-presence.ts`): presence
 * show mapping for 1:1 updates, MUC occupant cache fan-out, self-
 * presence join callbacks (XEP-0045 §7.2.2 / §7.6), and outbound
 * presence encoding — all without the full client.
 */
import { describe, expect, test } from "bun:test";
import { TypedEventBus, type ClientEvents } from "../src/lib/xmpp/client-events";
import { PresenceManager } from "../src/lib/xmpp/client-presence";
import type { PresenceUpdateEvent } from "../src/lib/xmpp/types";
import type { WasmPresence } from "../src/lib/xmpp/wasm-types";

const ROOM = "general@muc.example.com";

function createManager(overrides: {
  currentRoom?: () => string | null;
  onOwnSelfPresence?: (roomJid: string) => void;
  onOwnUnavailable?: (roomJid: string) => void;
  sendPresence?: (status?: string | null, show?: string | null, idleSince?: string | null) => Promise<unknown>;
} = {}) {
  const events = new TypedEventBus<ClientEvents>();
  const manager = new PresenceManager({
    events,
    currentRoom: overrides.currentRoom ?? (() => ROOM),
    ownFullJidCandidates: () => new Set(["alice@example.com/web-1"]),
    requireConnectedXmpp: async () => ({
      send_presence: overrides.sendPresence ?? (async () => undefined),
    }),
    handleMucPresenceError: () => false,
    onOwnSelfPresence: overrides.onOwnSelfPresence ?? (() => undefined),
    onOwnUnavailable: overrides.onOwnUnavailable ?? (() => undefined),
  });
  return { manager, events };
}

function directPresence(partial: Partial<WasmPresence>): WasmPresence {
  return { presence_type: "available", hats: [], ...partial };
}

describe("PresenceManager 1:1 show mapping", () => {
  test.each([
    ["dnd", "dnd"],
    ["away", "away"],
    ["xa", "xa"],
    ["chat", "available"],
    [undefined, "available"],
  ] as const)("show %p maps to %p", (show, expected) => {
    const { manager, events } = createManager();
    const updates: PresenceUpdateEvent[] = [];
    events.on("presenceUpdate", (event) => updates.push(event));

    manager.handle(directPresence({ from: "bob@example.com/phone", ...(show ? { show } : {}) }));

    expect(updates).toHaveLength(1);
    expect(updates[0].bareJid).toBe("bob@example.com");
    expect(updates[0].resource).toBe("phone");
    expect(updates[0].show).toBe(expected);
  });

  test("unavailable maps to offline and XEP-0319 idle is surfaced in epoch ms", () => {
    const { manager, events } = createManager();
    const updates: PresenceUpdateEvent[] = [];
    events.on("presenceUpdate", (event) => updates.push(event));

    manager.handle(directPresence({
      from: "bob@example.com/phone",
      presence_type: "unavailable",
      idle_since: "2024-01-01T00:00:00.000Z",
      status: "brb",
    }));

    expect(updates[0].show).toBe("offline");
    expect(updates[0].idleSince).toBe(Date.parse("2024-01-01T00:00:00.000Z"));
    expect(updates[0].status).toBe("brb");
  });
});

describe("PresenceManager MUC occupant tracking", () => {
  test("focused-room occupant presence fans out hats, authority, presence, and member JID", () => {
    const { manager, events } = createManager();
    const emitted: Record<string, unknown[]> = { hats: [], authority: [], presence: [], memberJid: [] };
    events.on("hats", (hats) => emitted.hats.push(hats));
    events.on("authority", (authority) => emitted.authority.push(authority));
    events.on("presence", (presence) => emitted.presence.push(presence));
    events.on("memberJid", (nick, bare) => emitted.memberJid.push([nick, bare]));

    manager.handle(directPresence({
      from: `${ROOM}/bob`,
      show: "dnd",
      hats: [{ uri: "urn:example:hats:mod", title: "Moderator" }],
      muc_affiliation: "member",
      muc_role: "moderator",
      muc_jid: "bob@example.com/phone",
    }));

    expect(emitted.hats.at(-1)).toEqual({ bob: [{ uri: "urn:example:hats:mod", title: "Moderator" }] });
    expect(emitted.authority.at(-1)).toEqual({ bob: { affiliation: "member", role: "moderator" } });
    expect(emitted.presence.at(-1)).toEqual({ bob: "dnd" });
    expect(emitted.memberJid).toEqual([["bob", "bob@example.com"]]);
  });

  test("unfocused-room presence is cached but not emitted; dispatchFocusedRoom replays it after a switch", () => {
    let focused: string | null = "other@muc.example.com";
    const { manager, events } = createManager({ currentRoom: () => focused });
    const presenceEmits: unknown[] = [];
    events.on("presence", (presence) => presenceEmits.push(presence));

    manager.handle(directPresence({ from: `${ROOM}/bob`, muc_affiliation: "member", muc_role: "participant" }));
    expect(presenceEmits).toHaveLength(0);

    focused = ROOM;
    manager.dispatchFocusedRoom();
    expect(presenceEmits.at(-1)).toEqual({ bob: "online" });
  });

  test("self-presence 110 resolves the join; unavailable revokes unless it is a §7.6 nick change", () => {
    const selfJoined: string[] = [];
    const revoked: string[] = [];
    const { manager } = createManager({
      onOwnSelfPresence: (roomJid) => selfJoined.push(roomJid),
      onOwnUnavailable: (roomJid) => revoked.push(roomJid),
    });

    manager.handle(directPresence({ from: `${ROOM}/alice`, muc_status_codes: [110] }));
    expect(selfJoined).toEqual([ROOM]);

    // Nick change: unavailable + 110 + 303 must NOT revoke readiness.
    manager.handle(directPresence({
      from: `${ROOM}/alice`,
      presence_type: "unavailable",
      muc_status_codes: [110, 303],
    }));
    expect(revoked).toEqual([]);

    manager.handle(directPresence({
      from: `${ROOM}/alice`,
      presence_type: "unavailable",
      muc_status_codes: [110],
    }));
    expect(revoked).toEqual([ROOM]);
  });

  test("occupant departure marks them offline and stamps lastSeen", () => {
    const { manager, events } = createManager();
    const presenceEmits: unknown[] = [];
    const lastSeen: string[] = [];
    events.on("presence", (presence) => presenceEmits.push(presence));
    events.on("lastSeen", (nick) => lastSeen.push(nick));

    manager.handle(directPresence({ from: `${ROOM}/bob`, muc_affiliation: "member", muc_role: "participant" }));
    manager.handle(directPresence({ from: `${ROOM}/bob`, presence_type: "unavailable", muc_affiliation: "member" }));

    expect(presenceEmits.at(-1)).toEqual({ bob: "offline" });
    expect(lastSeen).toEqual(["bob"]);
  });
});

describe("PresenceManager outbound presence", () => {
  test("setPresence omits the show element for available and stamps XEP-0319 idle", async () => {
    const sends: Array<[string | null | undefined, string | null | undefined, string | null | undefined]> = [];
    const { manager } = createManager({
      sendPresence: async (status, show, idleSince) => {
        sends.push([status, show, idleSince]);
        return undefined;
      },
    });

    await manager.setPresence("available");
    await manager.setPresence("dnd", Date.parse("2024-01-01T00:00:00.000Z"));

    expect(sends).toEqual([
      [undefined, undefined, undefined],
      [undefined, "dnd", "2024-01-01T00:00:00.000Z"],
    ]);
  });
});
