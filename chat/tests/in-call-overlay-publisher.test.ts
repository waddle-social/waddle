import { describe, expect, test } from "bun:test";

import { CallOverlayPublisher } from "../src/lib/calls/in-call-overlay";
import type { CallState } from "../src/lib/calls/types";
import type { ActivityPublication } from "../src/lib/xmpp/pep-types";

const JOIN = { url: "wss://sfu", room: "r", identity: "me@waddle.test/x", token: "t" };
const IDLE: CallState = { phase: "idle" };

function active(media: { audio: boolean; video: boolean }): CallState {
  return { phase: "active", peer: "alice@waddle.test/y", sid: "s1", media, join: JOIN, kind: "dm" };
}

type Emitted = { type: "publish"; activity: ActivityPublication } | { type: "retract" };

function harness() {
  const events: Emitted[] = [];
  const publisher = new CallOverlayPublisher({
    publish: (activity) => events.push({ type: "publish", activity }),
    retract: () => events.push({ type: "retract" }),
  });
  return { publisher, events };
}

describe("CallOverlayPublisher", () => {
  test("joining an audio call publishes the on_the_phone overlay once", () => {
    const { publisher, events } = harness();
    publisher.update(active({ audio: true, video: false }));
    expect(events).toEqual([
      { type: "publish", activity: { general: "talking", specific: "on_the_phone" } },
    ]);
  });

  test("leaving the call retracts the overlay", () => {
    const { publisher, events } = harness();
    publisher.update(active({ audio: true, video: false }));
    publisher.update(IDLE);
    expect(events.at(-1)).toEqual({ type: "retract" });
  });

  test("staying in the same call does not re-publish", () => {
    const { publisher, events } = harness();
    publisher.update(active({ audio: true, video: false }));
    publisher.update(active({ audio: true, video: false }));
    publisher.update(active({ audio: true, video: false }));
    expect(events).toHaveLength(1);
  });

  test("toggling video on mid-call re-publishes on_video_phone", () => {
    const { publisher, events } = harness();
    publisher.update(active({ audio: true, video: false }));
    publisher.update(active({ audio: true, video: true }));
    expect(events).toEqual([
      { type: "publish", activity: { general: "talking", specific: "on_the_phone" } },
      { type: "publish", activity: { general: "talking", specific: "on_video_phone" } },
    ]);
  });

  test("never retracts an overlay it never published (idle stays silent)", () => {
    const { publisher, events } = harness();
    publisher.update(IDLE);
    publisher.update({ phase: "outgoing", to: "alice@waddle.test", sid: "s", media: { audio: true, video: false } });
    publisher.update(IDLE);
    expect(events).toEqual([]);
  });

  test("ending the call retracts exactly once across ended→idle", () => {
    const { publisher, events } = harness();
    publisher.update(active({ audio: true, video: false }));
    publisher.update({ phase: "ended", sid: "s1", reason: null });
    publisher.update(IDLE);
    expect(events.filter((e) => e.type === "retract")).toHaveLength(1);
  });

  test("reset() forgets the wire baseline so the next session re-publishes", () => {
    const { publisher, events } = harness();
    publisher.update(active({ audio: true, video: false }));
    // Logout: the wire baseline is no longer trustworthy (new session/account).
    publisher.reset();
    publisher.update(active({ audio: true, video: false }));
    expect(events.filter((e) => e.type === "publish")).toHaveLength(2);
  });

  test("reassert re-publishes the live overlay (session-ready self-heal)", () => {
    const { publisher, events } = harness();
    // A publish that never reached the wire (client briefly null on connect)
    // still advanced the baseline; session-ready must re-assert it.
    publisher.reassert(active({ audio: true, video: false }));
    expect(events).toEqual([
      { type: "publish", activity: { general: "talking", specific: "on_the_phone" } },
    ]);
  });

  test("reassert is silent when not in a call (no empty-retraction spam)", () => {
    const { publisher, events } = harness();
    publisher.reassert(IDLE);
    expect(events).toEqual([]);
  });

  test("reassert refreshes the baseline so a later leave still retracts once", () => {
    const { publisher, events } = harness();
    publisher.update(active({ audio: true, video: false }));
    publisher.reassert(active({ audio: true, video: false })); // reconnect mid-call
    publisher.update(IDLE);
    expect(events.filter((e) => e.type === "publish")).toHaveLength(2);
    expect(events.filter((e) => e.type === "retract")).toHaveLength(1);
  });
});
