import { describe, expect, test } from "bun:test";
import { matchLocation } from "../src/router/match";
import { buildHref } from "../src/router/navigate";

describe("unread route", () => {
  test("matches /unread to the unread route id", () => {
    expect(matchLocation("/unread")).toEqual({ id: "unread" });
  });

  test("builds the canonical /unread href", () => {
    expect(buildHref({ id: "unread" })).toBe("/unread");
  });

  test("does not shadow other static routes", () => {
    expect(matchLocation("/threads")).toEqual({ id: "threads" });
    expect(matchLocation("/")).toEqual({ id: "home" });
  });
});
