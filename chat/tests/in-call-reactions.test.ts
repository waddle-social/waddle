import { describe, expect, test } from "bun:test";

import {
  reduceInCallReactions,
  type InCallReactionAnimation,
} from "../src/lib/calls/in-call-reactions";

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
});
