import { describe, expect, test } from "bun:test";
import { matchLocation } from "../src/router/match";
import { buildHref } from "../src/router/navigate";

describe("rooms route", () => {
  test("matches /rooms to the rooms route id", () => {
    expect(matchLocation("/rooms")).toEqual({ id: "rooms" });
  });

  test("builds the canonical /rooms href", () => {
    expect(buildHref({ id: "rooms" })).toBe("/rooms");
  });

  test("does not shadow other static routes", () => {
    expect(matchLocation("/members")).toEqual({ id: "members" });
    expect(matchLocation("/unread")).toEqual({ id: "unread" });
    expect(matchLocation("/")).toEqual({ id: "home" });
  });
});
