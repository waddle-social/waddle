import { describe, expect, test } from "bun:test";
import { matchLocation } from "../src/router/match";
import { buildHref } from "../src/router/navigate";

describe("members route", () => {
  test("matches /members to the members route id", () => {
    expect(matchLocation("/members")).toEqual({ id: "members" });
  });

  test("builds the canonical /members href", () => {
    expect(buildHref({ id: "members" })).toBe("/members");
  });

  test("does not shadow other static routes", () => {
    expect(matchLocation("/rooms")).toEqual({ id: "rooms" });
    expect(matchLocation("/threads")).toEqual({ id: "threads" });
    expect(matchLocation("/")).toEqual({ id: "home" });
  });
});
