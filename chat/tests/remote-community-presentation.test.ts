import { describe, expect, test } from "bun:test";
import {
  addressErrorMessage,
  departureNote,
  memberIdentityLabel,
  placeholderInitial,
  roomFlagLabels,
  statusLine,
} from "../src/waddles/remote-community-presentation";

describe("status lines", () => {
  test("an outage counts down to its automatic retry", () => {
    const line = statusLine(
      { state: "failed", failure: { kind: "outage", retry: "automatic" }, retryAt: 4_500 },
      "room",
      1_000,
    );
    expect(line).toMatchObject({ tone: "warning", canRetry: true, retryInSeconds: 4 });
  });

  test("a denial offers a retry the user starts, with no countdown", () => {
    const line = statusLine(
      { state: "failed", failure: { kind: "denial", retry: "manual", condition: "forbidden" }, retryAt: null },
      "room",
      0,
    );
    expect(line).toMatchObject({ tone: "danger", canRetry: true, retryInSeconds: null });
  });

  test("a missing community offers no retry", () => {
    const line = statusLine({ state: "failed", failure: { kind: "deletion", retry: "none" }, retryAt: null }, "space", 0);
    expect(line).toMatchObject({ tone: "danger", canRetry: false });
  });

  test("ready and idle entries show no status", () => {
    expect(statusLine({ state: "ready" }, "room", 0)).toBeNull();
    expect(statusLine({ state: "idle" }, "room", 0)).toBeNull();
  });
});

describe("labels", () => {
  test("address errors are explained", () => {
    expect(addressErrorMessage("resource")).toContain("without a nickname");
    expect(addressErrorMessage("refused-domain")).toContain("IP addresses");
  });

  test("a revocation is told apart from a kick and an outage", () => {
    const notes = new Set([departureNote("revoked"), departureNote("kicked"), departureNote("outage")]);
    expect(notes.size).toBe(3);
    expect(departureNote("left")).toBeNull();
  });

  test("room flags are listed as the room states them", () => {
    expect(roomFlagLabels({ membersOnly: true, passwordProtected: false, hidden: false, moderated: false, nonAnonymous: true }))
      .toEqual(["Members only", "Everyone there sees your address"]);
  });

  test("an image placeholder uses the first character, never a URL", () => {
    expect(placeholderInitial("engineering", "eng")).toBe("E");
    expect(placeholderInitial(null, "ops")).toBe("O");
  });

  test("a member elsewhere is named by full address", () => {
    expect(memberIdentityLabel({ jid: "alice@home.test", username: "alice" }, "home.test")).toBe("alice");
    expect(memberIdentityLabel({ jid: "alice@remote.test/phone", username: "alice" }, "home.test")).toBe("alice@remote.test");
  });
});
