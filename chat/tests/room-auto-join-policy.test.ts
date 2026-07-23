import { describe, expect, test } from "bun:test";
import {
  reconcileAutoJoinBlocks,
  type RoomAutoJoinBlock,
} from "../src/lib/xmpp/room-auto-join-policy";
import type { DiscoveredChannel } from "../src/lib/xmpp/types";

const roomJid = "private@muc.example.com";

function room(partial: Partial<DiscoveredChannel> = {}): DiscoveredChannel {
  return {
    id: "private",
    name: "Private",
    jid: roomJid,
    channelType: "chat",
    position: 0,
    autojoin: true,
    ...partial,
  };
}

function deniedBlock(
  partial: Partial<RoomAutoJoinBlock> = {},
): ReadonlyMap<string, RoomAutoJoinBlock> {
  return new Map([[
    roomJid,
    {
      roomJid,
      condition: "registration-required",
      ...partial,
    },
  ]]);
}

describe("room auto-join terminal authorization policy", () => {
  test("an identical first catalog observation preserves a restored denial", () => {
    const first = reconcileAutoJoinBlocks(deniedBlock(), [
      room({ jid: "Private@MUC.Example.com" }),
    ]);
    expect(first.unblockedRoomKeys).toEqual([]);
    expect(first.blocks.get(roomJid)?.catalogFingerprint).toBeString();

    const refresh = reconcileAutoJoinBlocks(first.blocks, [room()]);
    expect(refresh.unblockedRoomKeys).toEqual([]);
    expect(refresh.blocks.has(roomJid)).toBe(true);
  });

  test("a bookmark membership change restores auto-join eligibility", () => {
    const first = reconcileAutoJoinBlocks(deniedBlock(), [room()]);
    const changed = reconcileAutoJoinBlocks(first.blocks, [
      room({ isBookmarked: true }),
    ]);

    expect(changed.unblockedRoomKeys).toEqual([roomJid]);
    expect(changed.blocks.has(roomJid)).toBe(false);
  });

  test("a first bookmarked observation unblocks a pre-discovery denial", () => {
    const reconciled = reconcileAutoJoinBlocks(deniedBlock(), [
      room({ isBookmarked: true }),
    ]);

    expect(reconciled.unblockedRoomKeys).toEqual([roomJid]);
    expect(reconciled.blocks.has(roomJid)).toBe(false);
  });
});
