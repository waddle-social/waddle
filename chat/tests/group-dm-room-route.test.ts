import { describe, expect, test } from "bun:test";
import { groupDmRoomRoute } from "../src/router/routes/group-dm-room";

describe("groupDmRoomRoute", () => {
  test("builds an encoded route with the room bare JID as identity", () => {
    expect(groupDmRoomRoute.href({
      params: { roomJid: "group-dm-rock@muc.localhost" },
      search: { thread: ["root"], pinned: true },
    })).toBe("/dm/room/group-dm-rock%40muc.localhost?thread=root&pinned=1");
  });

  test("parses the encoded room bare JID", () => {
    expect(groupDmRoomRoute.tryParse("/dm/room/group-dm-rock%40muc.localhost", "?pinned=1"))
      .toEqual({
        id: "groupDmRoom",
        params: { roomJid: "group-dm-rock@muc.localhost" },
        search: { thread: [], pinned: true },
      });
  });
});
