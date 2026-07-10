import { describe, expect, mock, test } from "bun:test";
import { createMucRoom } from "../src/lib/xmpp/protocol-helpers";

describe("protocol helpers", () => {
  test("creates MUC rooms via join_room (always no-history per #1255)", async () => {
    const joinRoom = mock(async () => undefined);
    const leaveRoom = mock(async () => undefined);
    const sendRawIq = mock(async () => "<iq type='result'/>");
    const client = {
      join_room: joinRoom,
      leave_room: leaveRoom,
      send_raw_iq: sendRawIq,
    };

    await createMucRoom(client, "rooms.example.com", {
      roomLocalpart: "general",
      nick: "alice",
      name: "General",
      description: "Default room",
    });

    // #1255: `join_room` itself sends `<history maxstanzas='0'/>`
    // (asserted in the Rust XEP-0045 suite), so the helper needs no
    // separate no-history variant.
    expect(joinRoom).toHaveBeenCalledWith("general@rooms.example.com", "alice");
    expect(sendRawIq).toHaveBeenCalledTimes(1);
    expect(leaveRoom).toHaveBeenCalledWith("general@rooms.example.com", "alice");
  });
});
