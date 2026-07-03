import { describe, expect, test } from "bun:test";
import {
  formatReactors,
  isReactor,
  reactionAriaLabel,
  reactionWarmthClass,
} from "../src/components/chat/message-reaction-chips";

describe("reactionWarmthClass", () => {
  test("tiers scale with reaction count", () => {
    expect(reactionWarmthClass(1)).toBe("");
    expect(reactionWarmthClass(2)).toBe("chat-reaction-chip--warmth-tepid");
    expect(reactionWarmthClass(3)).toBe("chat-reaction-chip--warmth-warm");
    expect(reactionWarmthClass(4)).toBe("chat-reaction-chip--warmth-warm");
    expect(reactionWarmthClass(5)).toBe("chat-reaction-chip--warmth-hot");
    expect(reactionWarmthClass(42)).toBe("chat-reaction-chip--warmth-hot");
  });
});

describe("formatReactors", () => {
  test("joins nicks as a conjunction list", () => {
    expect(formatReactors(["alice"])).toBe("alice");
    expect(formatReactors(["alice", "bob", "carol"])).toContain("alice");
    expect(formatReactors(["alice", "bob", "carol"])).toContain("carol");
  });
});

describe("reactionAriaLabel", () => {
  test("describes who reacted with what", () => {
    expect(reactionAriaLabel("🎉", ["alice"])).toBe("alice reacted with 🎉");
  });
});

describe("isReactor", () => {
  test("matches the current user's nick exactly", () => {
    expect(isReactor(["alice", "bob"], "alice")).toBe(true);
    expect(isReactor(["alice", "bob"], "carol")).toBe(false);
    expect(isReactor(["alice"], undefined)).toBe(false);
  });
});
