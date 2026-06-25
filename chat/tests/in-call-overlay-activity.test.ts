import { describe, expect, test } from "bun:test";

import { callOverlayActivity } from "../src/lib/calls/in-call-overlay";
import type { CallState } from "../src/lib/calls/types";

const JOIN = { url: "wss://sfu", room: "r", identity: "me@waddle.test/x", token: "t" };

function activeCall(media: { audio: boolean; video: boolean }, kind: "dm" | "muc" = "dm"): CallState {
  return { phase: "active", peer: "alice@waddle.test/y", sid: "s1", media, join: JOIN, kind };
}

describe("callOverlayActivity", () => {
  test("an active audio call publishes XEP-0108 talking/on_the_phone", () => {
    expect(callOverlayActivity(activeCall({ audio: true, video: false }))).toEqual({
      general: "talking",
      specific: "on_the_phone",
    });
  });

  test("an active video call publishes talking/on_video_phone", () => {
    expect(callOverlayActivity(activeCall({ audio: true, video: true }))).toEqual({
      general: "talking",
      specific: "on_video_phone",
    });
  });

  test("a MUC group call publishes the same overlay as a 1:1 call", () => {
    expect(callOverlayActivity(activeCall({ audio: true, video: false }, "muc"))).toEqual({
      general: "talking",
      specific: "on_the_phone",
    });
  });

  test.each([
    ["idle", { phase: "idle" }],
    ["incoming", { phase: "incoming", from: "a@waddle.test/r", sid: "s", media: { audio: true, video: false } }],
    ["outgoing", { phase: "outgoing", to: "a@waddle.test", sid: "s", media: { audio: true, video: false } }],
    ["muc-pending", { phase: "muc-pending", peer: "room@conf", sid: "s", media: { audio: true, video: false }, kind: "muc", selfNick: "me", attemptId: "a" }],
    ["ended", { phase: "ended", sid: "s", reason: null }],
  ] as const)("the %s phase publishes no overlay (retract)", (_name, state) => {
    expect(callOverlayActivity(state as CallState)).toBeNull();
  });
});
