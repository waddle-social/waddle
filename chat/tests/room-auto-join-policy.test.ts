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
    ], { absentRoomKeysAuthoritative: true });
    expect(first.unblockedRoomKeys).toEqual([]);
    expect(first.blocks.get(roomJid)?.catalogFingerprint).toBeString();

    const refresh = reconcileAutoJoinBlocks(first.blocks, [room()], {
      absentRoomKeysAuthoritative: true,
    });
    expect(refresh.unblockedRoomKeys).toEqual([]);
    expect(refresh.blocks.has(roomJid)).toBe(true);
  });

  test("a bookmark membership change restores auto-join eligibility", () => {
    const first = reconcileAutoJoinBlocks(deniedBlock(), [room()], {
      absentRoomKeysAuthoritative: true,
    });
    const changed = reconcileAutoJoinBlocks(first.blocks, [
      room({ isBookmarked: true }),
    ], { absentRoomKeysAuthoritative: true });

    expect(changed.unblockedRoomKeys).toEqual([roomJid]);
    expect(changed.blocks.has(roomJid)).toBe(false);
  });

  test("a first bookmarked observation records a baseline instead of unblocking", () => {
    const first = reconcileAutoJoinBlocks(deniedBlock(), [
      room({ isBookmarked: true }),
    ], { absentRoomKeysAuthoritative: true });

    expect(first.unblockedRoomKeys).toEqual([]);
    expect(first.blocks.get(roomJid)?.catalogFingerprint).toBeString();

    const changed = reconcileAutoJoinBlocks(first.blocks, [
      room({ isBookmarked: false }),
    ], { absentRoomKeysAuthoritative: true });

    expect(changed.unblockedRoomKeys).toEqual([roomJid]);
    expect(changed.blocks.has(roomJid)).toBe(false);
  });

  test("an incomplete catalog cannot clear a fingerprinted denial", () => {
    const baseline = reconcileAutoJoinBlocks(deniedBlock(), [room()], {
      absentRoomKeysAuthoritative: true,
    });
    const incomplete = reconcileAutoJoinBlocks(
      baseline.blocks,
      [],
      { absentRoomKeysAuthoritative: false },
    );

    expect(incomplete.unblockedRoomKeys).toEqual([]);
    expect(incomplete.blocks).toEqual(baseline.blocks);
    expect(incomplete.changed).toBe(false);
  });

  test("an authoritative room change can unblock while unrelated discovery is incomplete", () => {
    const baseline = reconcileAutoJoinBlocks(deniedBlock(), [room()], {
      absentRoomKeysAuthoritative: true,
    });
    const changed = reconcileAutoJoinBlocks(
      baseline.blocks,
      [room({ isBookmarked: true })],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeRoomKeys: new Set([roomJid]),
      },
    );

    expect(changed.unblockedRoomKeys).toEqual([roomJid]);
    expect(changed.blocks.has(roomJid)).toBe(false);
  });

  test("an incomplete fingerprint preserves its room even when a sibling is authoritative", () => {
    const baseline = reconcileAutoJoinBlocks(deniedBlock(), [room()], {
      absentRoomKeysAuthoritative: true,
    });
    const incomplete = reconcileAutoJoinBlocks(
      baseline.blocks,
      [room({ isBookmarked: true })],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeRoomKeys: new Set(["sibling@muc.example.com"]),
      },
    );

    expect(incomplete.unblockedRoomKeys).toEqual([]);
    expect(incomplete.blocks).toEqual(baseline.blocks);
    expect(incomplete.changed).toBe(false);
  });

  test("authoritative absence unblocks a room removed from the membership catalog", () => {
    const baseline = reconcileAutoJoinBlocks(deniedBlock(), [room()], {
      absentRoomKeysAuthoritative: true,
    });
    const removed = reconcileAutoJoinBlocks(
      baseline.blocks,
      [],
      {
        absentRoomKeysAuthoritative: true,
        authoritativeRoomKeys: new Set(),
      },
    );

    expect(removed.unblockedRoomKeys).toEqual([roomJid]);
    expect(removed.blocks.has(roomJid)).toBe(false);
  });
});
