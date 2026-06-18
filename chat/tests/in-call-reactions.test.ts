import { describe, expect, test } from "bun:test";

import {
  activeInCallReactions,
  hashInCallReactionId,
  isInCallReactionForActiveCall,
  reduceInCallReactions,
  type InCallReactionAnimation,
} from "../src/lib/calls/in-call-reactions";
import type { CallState } from "../src/lib/calls/types";

describe("in-call reaction reducer", () => {
  test("adds received reactions and expires them by timestamp", () => {
    const first = reduceInCallReactions([], {
      kind: "received",
      sid: "call-1",
      emoji: "🔥",
      from: "alice@example.com/phone",
      now: 1_000,
    });

    expect(first).toHaveLength(1);
    expect(first[0]).toMatchObject({
      sid: "call-1",
      emoji: "🔥",
      from: "alice@example.com/phone",
      createdAt: 1_000,
      expiresAt: 3_400,
    } satisfies Partial<InCallReactionAnimation>);

    const second = reduceInCallReactions(first, {
      kind: "received",
      sid: "call-1",
      emoji: "👍",
      from: "bob@example.com/laptop",
      now: 1_100,
    });
    expect(second.map((reaction) => reaction.emoji)).toEqual(["🔥", "👍"]);

    const expired = reduceInCallReactions(second, {
      kind: "expire",
      now: 3_499,
    });
    expect(expired.map((reaction) => reaction.emoji)).toEqual(["👍"]);
  });

  test("filters and accepts reactions only for the active call", () => {
    const activeCall = {
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/laptop",
      sid: "call-1",
      media: { audio: true, video: false },
      join: {
        url: "wss://livekit.example.com",
        token: "token",
        room: "room",
      },
    } satisfies CallState;
    const reactions: InCallReactionAnimation[] = [
      {
        id: "call-1:1000:0",
        sid: "call-1",
        emoji: "🔥",
        from: "alice@example.com/phone",
        createdAt: 1_000,
        expiresAt: 3_400,
      },
      {
        id: "call-2:1000:1",
        sid: "call-2",
        emoji: "👍",
        from: "bob@example.com/laptop",
        createdAt: 1_000,
        expiresAt: 3_400,
      },
    ];

    expect(isInCallReactionForActiveCall(activeCall, "call-1")).toBe(true);
    expect(isInCallReactionForActiveCall(activeCall, "call-2")).toBe(false);
    expect(activeInCallReactions(activeCall, reactions).map((reaction) => reaction.emoji)).toEqual([
      "🔥",
    ]);
    expect(activeInCallReactions({ phase: "idle" }, reactions)).toEqual([]);
  });

  test("hashes reaction ids consistently", () => {
    expect(hashInCallReactionId("call-1:1000:0")).toBe(hashInCallReactionId("call-1:1000:0"));
    expect(hashInCallReactionId("call-1:1000:0")).not.toBe(hashInCallReactionId("call-2:1000:0"));
  });
});
