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
    ], { catalogComplete: true });
    expect(first.unblockedRoomKeys).toEqual([]);
    expect(first.blocks.get(roomJid)?.catalogFingerprint).toBeString();

    const refresh = reconcileAutoJoinBlocks(first.blocks, [room()], {
      catalogComplete: true,
    });
    expect(refresh.unblockedRoomKeys).toEqual([]);
    expect(refresh.blocks.has(roomJid)).toBe(true);
  });

  test("a bookmark membership change restores auto-join eligibility", () => {
    const first = reconcileAutoJoinBlocks(deniedBlock(), [room()], {
      catalogComplete: true,
    });
    const changed = reconcileAutoJoinBlocks(first.blocks, [
      room({ isBookmarked: true }),
    ], { catalogComplete: true });

    expect(changed.unblockedRoomKeys).toEqual([roomJid]);
    expect(changed.blocks.has(roomJid)).toBe(false);
  });

  test("a first bookmarked observation records a baseline instead of unblocking", () => {
    const first = reconcileAutoJoinBlocks(deniedBlock(), [
      room({ isBookmarked: true }),
    ], { catalogComplete: true });

    expect(first.unblockedRoomKeys).toEqual([]);
    expect(first.blocks.get(roomJid)?.catalogFingerprint).toBeString();

    const changed = reconcileAutoJoinBlocks(first.blocks, [
      room({ isBookmarked: false }),
    ], { catalogComplete: true });

    expect(changed.unblockedRoomKeys).toEqual([roomJid]);
    expect(changed.blocks.has(roomJid)).toBe(false);
  });

  test("an incomplete catalog cannot clear a fingerprinted denial", () => {
    const baseline = reconcileAutoJoinBlocks(deniedBlock(), [room()], {
      catalogComplete: true,
    });
    const incomplete = reconcileAutoJoinBlocks(
      baseline.blocks,
      [],
      { catalogComplete: false },
    );

    expect(incomplete.unblockedRoomKeys).toEqual([]);
    expect(incomplete.blocks).toEqual(baseline.blocks);
    expect(incomplete.changed).toBe(false);
  });
});
