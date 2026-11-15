import { describe, expect, test } from "bun:test";
import {
  formatReactors,
  isReactor,
  reactionAriaLabel,
} from "../src/components/chat/message-reaction-chips";

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
