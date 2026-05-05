import { describe, expect, mock, test } from "bun:test";
import { createMucRoom } from "../src/lib/xmpp/protocol-helpers";

describe("protocol helpers", () => {
  test("creates MUC rooms with the Rust no-history join path", async () => {
    const joinRoomWithoutHistory = mock(async () => undefined);
    const joinRoom = mock(async () => undefined);
    const leaveRoom = mock(async () => undefined);
    const sendRawIq = mock(async () => "<iq type='result'/>");
    const client = {
      join_room_without_history: joinRoomWithoutHistory,
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

    expect(joinRoomWithoutHistory).toHaveBeenCalledWith("general@rooms.example.com", "alice");
    expect(joinRoom).not.toHaveBeenCalled();
    expect(sendRawIq).toHaveBeenCalledTimes(1);
    expect(leaveRoom).toHaveBeenCalledWith("general@rooms.example.com", "alice");
  });
});
