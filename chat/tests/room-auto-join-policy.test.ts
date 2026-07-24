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
        authoritativeFingerprintFields: new Map([[
          roomJid,
          new Set(["isBookmarked"] as const),
        ]]),
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
        authoritativeFingerprintFields: new Map([[
          "sibling@muc.example.com",
          new Set(["isBookmarked"] as const),
        ]]),
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
        authoritativeFingerprintFields: new Map(),
      },
    );

    expect(removed.unblockedRoomKeys).toEqual([roomJid]);
    expect(removed.blocks.has(roomJid)).toBe(false);
  });

  test("partial bookmark authority compares only the fields its source proved", () => {
    const baseline = reconcileAutoJoinBlocks(deniedBlock(), [
      room({ spaceId: "space-old", autojoin: true, isBookmarked: true }),
    ], { absentRoomKeysAuthoritative: true });
    const unrelatedAutojoinValue = reconcileAutoJoinBlocks(
      baseline.blocks,
      [room({
        spaceId: "space-old",
        autojoin: false,
        isBookmarked: true,
      })],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeFingerprintFields: new Map([[
          roomJid,
          new Set(["spaceId", "isBookmarked"] as const),
        ]]),
      },
    );

    expect(unrelatedAutojoinValue.unblockedRoomKeys).toEqual([]);
    expect(unrelatedAutojoinValue.blocks).toEqual(baseline.blocks);

    const changedSpace = reconcileAutoJoinBlocks(
      baseline.blocks,
      [room({
        spaceId: "space-new",
        autojoin: false,
        isBookmarked: true,
      })],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeFingerprintFields: new Map([[
          roomJid,
          new Set(["spaceId", "isBookmarked"] as const),
        ]]),
      },
    );

    expect(changedSpace.unblockedRoomKeys).toEqual([roomJid]);
  });

  test("a first partial observation records only proven fingerprint fields", () => {
    const partial = reconcileAutoJoinBlocks(
      deniedBlock(),
      [room({ spaceId: "space-new", isBookmarked: true })],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeFingerprintFields: new Map([[
          roomJid,
          new Set(["spaceId", "isBookmarked"] as const),
        ]]),
      },
    );

    expect(partial.unblockedRoomKeys).toEqual([]);
    expect(partial.blocks.get(roomJid)?.catalogFingerprint).toBeString();
    expect(partial.blocks.get(roomJid)?.catalogFingerprintFields).toEqual([
      "spaceId",
      "isBookmarked",
    ]);
    expect(partial.changed).toBe(true);
  });

  test("a later change to a previously proven field unblocks after a partial baseline", () => {
    const partial = reconcileAutoJoinBlocks(
      deniedBlock(),
      [room({ spaceId: "space-old", isBookmarked: true })],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeFingerprintFields: new Map([[
          roomJid,
          new Set(["spaceId", "isBookmarked"] as const),
        ]]),
      },
    );
    const changed = reconcileAutoJoinBlocks(
      partial.blocks,
      [room({ spaceId: "space-new", isBookmarked: true })],
      { absentRoomKeysAuthoritative: true },
    );

    expect(changed.unblockedRoomKeys).toEqual([roomJid]);
    expect(changed.blocks.has(roomJid)).toBe(false);
  });

  test("newly proven fields extend a partial baseline without causing a false unblock", () => {
    const partial = reconcileAutoJoinBlocks(
      deniedBlock(),
      [room({
        spaceId: "space-old",
        autojoin: true,
        isBookmarked: true,
      })],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeFingerprintFields: new Map([[
          roomJid,
          new Set(["spaceId", "isBookmarked"] as const),
        ]]),
      },
    );
    const completed = reconcileAutoJoinBlocks(
      partial.blocks,
      [room({
        spaceId: "space-old",
        autojoin: false,
        isBookmarked: true,
      })],
      { absentRoomKeysAuthoritative: true },
    );

    expect(completed.unblockedRoomKeys).toEqual([]);
    expect(completed.blocks.has(roomJid)).toBe(true);
    expect(
      completed.blocks.get(roomJid)?.catalogFingerprintFields,
    ).toBeUndefined();

    const laterChange = reconcileAutoJoinBlocks(
      completed.blocks,
      [room({
        spaceId: "space-old",
        autojoin: true,
        isBookmarked: true,
      })],
      { absentRoomKeysAuthoritative: true },
    );
    expect(laterChange.unblockedRoomKeys).toEqual([roomJid]);
  });

  test("an unchanged complete observation promotes a partial baseline without unblocking", () => {
    const observedRoom = room({
      spaceId: "space-old",
      autojoin: true,
      isBookmarked: true,
    });
    const partial = reconcileAutoJoinBlocks(
      deniedBlock(),
      [observedRoom],
      {
        absentRoomKeysAuthoritative: false,
        authoritativeFingerprintFields: new Map([[
          roomJid,
          new Set(["spaceId", "isBookmarked"] as const),
        ]]),
      },
    );
    const completed = reconcileAutoJoinBlocks(
      partial.blocks,
      [observedRoom],
      { absentRoomKeysAuthoritative: true },
    );

    expect(completed.unblockedRoomKeys).toEqual([]);
    expect(completed.blocks.has(roomJid)).toBe(true);
    expect(
      completed.blocks.get(roomJid)?.catalogFingerprintFields,
    ).toBeUndefined();
    expect(completed.changed).toBe(true);
  });
});
