import { describe, expect, test } from "bun:test";

import { ActivityCoordinator, activityFlushForDisconnect } from "../src/presence/activity-coordinator";
import type { CallState } from "../src/lib/calls/types";
import type { ActivityPublication } from "../src/lib/xmpp/pep-types";

const JOIN = { url: "wss://sfu", room: "r", identity: "me@waddle.test/x", token: "t" };
const IDLE: CallState = { phase: "idle" };
const WORKING: ActivityPublication = { general: "working", specific: "coding" };
const ON_THE_PHONE = { general: "talking", specific: "on_the_phone" };

function active(media: { audio: boolean; video: boolean }): CallState {
  return { phase: "active", peer: "alice@waddle.test/y", sid: "s1", media, join: JOIN, kind: "dm" };
}

type Emitted = { type: "publish"; activity: ActivityPublication } | { type: "retract" };

function harness() {
  let call: CallState = IDLE;
  let manual: ActivityPublication | null = null;
  let online = true;
  const events: Emitted[] = [];
  const coord = new ActivityCoordinator({
    publish: (activity) => {
      if (!online) return false;
      events.push({ type: "publish", activity });
      return true;
    },
    retract: () => {
      if (!online) return false;
      events.push({ type: "retract" });
      return true;
    },
    callState: () => call,
    manualActivity: () => manual,
  });
  return {
    coord,
    events,
    setCall: (c: CallState) => { call = c; },
    setManual: (m: ActivityPublication | null) => { manual = m; },
    setOnline: (o: boolean) => { online = o; },
  };
}

describe("ActivityCoordinator", () => {
  test("publishes the manual activity when set and not in a call", () => {
    const h = harness();
    h.setManual(WORKING);
    h.coord.reconcile();
    expect(h.events).toEqual([{ type: "publish", activity: WORKING }]);
  });

  test("the in-call overlay overrides the manual activity while a call is active", () => {
    const h = harness();
    h.setManual(WORKING);
    h.coord.reconcile(); // publishes working
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile();
    expect(h.events.at(-1)).toEqual({ type: "publish", activity: ON_THE_PHONE });
  });

  test("leaving a call RESTORES the manual activity (not a blanket retract)", () => {
    const h = harness();
    h.setManual(WORKING);
    h.coord.reconcile();
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile();
    h.setCall(IDLE);
    h.coord.reconcile();
    expect(h.events.at(-1)).toEqual({ type: "publish", activity: WORKING });
  });

  test("leaving a call with no manual activity retracts", () => {
    const h = harness();
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile();
    h.setCall(IDLE);
    h.coord.reconcile();
    expect(h.events.at(-1)).toEqual({ type: "retract" });
  });

  test("changing manual activity mid-call doesn't touch the wire but is restored on leave", () => {
    const h = harness();
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile(); // on_the_phone
    h.setManual({ general: "eating" });
    h.coord.reconcile(); // overlay still wins — no change
    expect(h.events.at(-1)).toEqual({ type: "publish", activity: ON_THE_PHONE });
    h.setCall(IDLE);
    h.coord.reconcile();
    expect(h.events.at(-1)).toEqual({ type: "publish", activity: { general: "eating" } });
  });

  test("does not re-publish an unchanged effective activity", () => {
    const h = harness();
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile();
    h.coord.reconcile();
    h.coord.reconcile();
    expect(h.events).toHaveLength(1);
  });

  test("a write that isn't sent (offline) is retried on the next reconcile", () => {
    const h = harness();
    h.setOnline(false);
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile(); // publish attempted but not sent
    expect(h.events).toEqual([]);
    h.setOnline(true);
    h.coord.reconcile(); // session-ready self-heal
    expect(h.events).toEqual([{ type: "publish", activity: ON_THE_PHONE }]);
  });

  test("a retract lost during a stream drop is recovered on reconnect (no stale overlay)", () => {
    const h = harness();
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile(); // publish on_the_phone (sent)
    h.setOnline(false); // stream drops
    h.setCall(IDLE); // call ends during the drop
    h.coord.reconcile(); // retract attempted but swallowed (not sent)
    h.setOnline(true); // reconnect
    h.coord.reconcile();
    expect(h.events.at(-1)).toEqual({ type: "retract" });
  });

  test("reset forgets the wire baseline so the next session re-publishes", () => {
    const h = harness();
    h.setCall(active({ audio: true, video: false }));
    h.coord.reconcile();
    h.coord.reset();
    h.coord.reconcile();
    expect(h.events.filter((e) => e.type === "publish")).toHaveLength(2);
  });
});

// Graceful-disconnect ghost cleanup (ADR-010): the decision the logout path
// runs before the stream closes, so no stale "in a call" survives.
describe("activityFlushForDisconnect", () => {
  test("no active call: nothing to flush (the node already holds the manual activity)", () => {
    expect(activityFlushForDisconnect(false, WORKING)).toEqual({ kind: "none" });
    expect(activityFlushForDisconnect(false, null)).toEqual({ kind: "none" });
  });

  test("in a call with a manual activity: restore it before disconnect", () => {
    expect(activityFlushForDisconnect(true, WORKING)).toEqual({ kind: "publish", activity: WORKING });
  });

  test("in a call with no manual activity: retract the overlay before disconnect", () => {
    expect(activityFlushForDisconnect(true, null)).toEqual({ kind: "retract" });
  });
});
