/**
 * Unit tests for the MUC admin/membership module extracted from
 * `BrowserXmppClient` (`src/lib/xmpp/client-muc-admin.ts`):
 * affiliation-based member listing (XEP-0045 §9) including partial
 * failure classification, and the snake_case translation of the admin
 * V2 wrappers — exercised against a fake WASM client.
 */
import { describe, expect, test } from "bun:test";
import {
  MucAdmin,
  RoomMemberListUnavailableError,
  type MucAdminWasmClient,
} from "../src/lib/xmpp/client-muc-admin";
import type { XmppErrorEvent } from "../src/lib/xmpp/types";

function createAdmin(xmpp: MucAdminWasmClient) {
  const errors: XmppErrorEvent[] = [];
  const admin = new MucAdmin({
    requireConnectedXmpp: async () => xmpp,
    roomJidForChannel: (channelId) => `${channelId}@muc.example.com`,
    emitError: (event) => errors.push(event),
  });
  return { admin, errors };
}

describe("MucAdmin.listRoomMembers", () => {
  test("aggregates members across all four affiliation queries", async () => {
    const queried: string[] = [];
    const { admin, errors } = createAdmin({
      list_room_members: async (_roomJid, affiliation) => {
        queried.push(affiliation);
        return affiliation === "owner"
          ? [{ jid: "alice@example.com" }]
          : affiliation === "member"
            ? [{ jid: "bob@example.com" }]
            : [];
      },
    });

    const members = await admin.listRoomMembers("general");

    expect(queried).toEqual(["owner", "admin", "member", "outcast"]);
    expect(members).toEqual([
      { jid: "alice@example.com", username: "alice", avatar_url: null, affiliation: "owner", joined_at: "" },
      { jid: "bob@example.com", username: "bob", avatar_url: null, affiliation: "member", joined_at: "" },
    ]);
    expect(errors).toEqual([]);
  });

  test("partial failures surface recoverable member-query errors but keep the successful rows", async () => {
    const { admin, errors } = createAdmin({
      list_room_members: async (_roomJid, affiliation) => {
        if (affiliation === "outcast") throw { condition: "forbidden" };
        return affiliation === "member" ? [{ jid: "bob@example.com" }] : [];
      },
    });

    const members = await admin.listRoomMembers("general");

    expect(members.map((member) => member.jid)).toEqual(["bob@example.com"]);
    expect(errors).toHaveLength(1);
    expect(errors[0].kind).toBe("member-query");
    expect(errors[0].recoverable).toBe(true);
    expect(errors[0]?.kind === "member-query" ? errors[0].condition : undefined)
      .toBe("forbidden");
  });

  test("structured Error rejections from the wasm bridge surface condition, errorType, and text", async () => {
    const rejection = Object.assign(
      new Error("server returned a stanza error: cancel: item-not-found"),
      { condition: "item-not-found", errorType: "cancel", text: "no such room" },
    );
    const { admin, errors } = createAdmin({
      list_room_members: async (_roomJid, affiliation) => {
        if (affiliation === "owner") throw rejection;
        return affiliation === "member" ? [{ jid: "bob@example.com" }] : [];
      },
    });

    const members = await admin.listRoomMembers("general");

    expect(members.map((member) => member.jid)).toEqual(["bob@example.com"]);
    expect(errors).toHaveLength(1);
    const error = errors[0];
    if (error?.kind !== "member-query") throw new Error("expected member query error");
    expect(error.condition).toBe("item-not-found");
    expect(error.errorType).toBe("cancel");
    expect(error.errorText).toBe("no such room");
  });

  test("throws RoomMemberListUnavailableError when every affiliation query fails", async () => {
    const { admin } = createAdmin({
      list_room_members: async () => {
        throw { error: { condition: "service-unavailable" } };
      },
    });

    await expect(admin.listRoomMembers("general")).rejects.toBeInstanceOf(RoomMemberListUnavailableError);
  });
});

describe("MucAdmin admin V2 wrappers", () => {
  test("adminSpacesList passes snake_case args verbatim to the wasm binding", async () => {
    const calls: unknown[] = [];
    const { admin } = createAdmin({
      admin_spaces_list: async (args) => {
        calls.push(args);
        return { entries: [], next_cursor: null };
      },
    });

    await admin.adminSpacesList({ prefix: "dev", pageSize: 10, afterCursor: "c1" });

    expect(calls).toEqual([{ prefix: "dev", page_size: 10, after_cursor: "c1" }]);
  });

  test("missing bindings reject with an explicit error instead of silently no-oping", async () => {
    const { admin } = createAdmin({});
    await expect(admin.adminChannelsKick({ channelJid: "general@muc.example.com", occupantJid: "general@muc.example.com/mallory" }))
      .rejects.toThrow("admin_channels_kick binding missing");
  });
});
