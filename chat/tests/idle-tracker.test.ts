import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import type { PresenceMode } from "../src/presence/effective-show";
import type { BroadcastPresence } from "../src/presence/effective-show";
import { IdleTracker } from "../src/presence/idle-tracker";

const AWAY_MS = 10 * 60_000;
const XA_MS = 30 * 60_000;
const thresholds = { awayMs: AWAY_MS, xaMs: XA_MS };

// A controllable clock + mode so the emission logic is testable without a DOM.
function harness(initialMode: PresenceMode = { kind: "automatic" }) {
  let clock = 1_000_000;
  let mode = initialMode;
  const sent: BroadcastPresence[] = [];
  const tracker = new IdleTracker({
    now: () => clock,
    getMode: () => mode,
    onBroadcast: (b) => sent.push(b),
    thresholds,
  });
  return {
    tracker,
    sent,
    advance: (ms: number) => {
      clock += ms;
    },
    setMode: (m: PresenceMode) => {
      mode = m;
    },
    at: () => clock,
  };
}

describe("IdleTracker", () => {
  let h: ReturnType<typeof harness>;
  beforeEach(() => {
    h = harness();
  });

  test("broadcasts Away with an idle stamp once the threshold passes", () => {
    const stoppedAt = h.at();
    h.advance(AWAY_MS + 1);
    h.tracker.evaluate();
    expect(h.sent).toEqual([{ show: "away", idleSince: stoppedAt }]);
  });

  test("escalates to Extended Away, keeping the original idle instant", () => {
    const stoppedAt = h.at();
    h.advance(AWAY_MS + 1);
    h.tracker.evaluate();
    h.advance(XA_MS - AWAY_MS); // now past the xa threshold overall
    h.tracker.evaluate();
    expect(h.sent).toEqual([
      { show: "away", idleSince: stoppedAt },
      { show: "xa", idleSince: stoppedAt },
    ]);
  });

  test("any interaction restores Available and strips the idle stamp", () => {
    h.advance(AWAY_MS + 1);
    h.tracker.evaluate(); // away
    h.advance(5_000);
    h.tracker.markActive(); // user moved the mouse
    expect(h.sent).toEqual([
      { show: "away", idleSince: expect.any(Number) },
      { show: "available", idleSince: null },
    ]);
  });

  test("a manual status suspends auto-away — no broadcast past the threshold", () => {
    h.setMode({ kind: "manual", status: "available" });
    h.advance(XA_MS + 1);
    h.tracker.evaluate();
    expect(h.sent).toEqual([]);
  });

  test("staying Away across re-checks does not re-broadcast", () => {
    h.advance(AWAY_MS + 1);
    h.tracker.evaluate();
    h.advance(60_000);
    h.tracker.evaluate();
    h.advance(60_000);
    h.tracker.evaluate();
    expect(h.sent.filter((p) => p.show === "away")).toHaveLength(1);
  });

  test("current() reflects a pinned manual status with no idle (reconnect path)", () => {
    h.setMode({ kind: "manual", status: "dnd" });
    h.advance(XA_MS + 1);
    expect(h.tracker.current()).toEqual({ show: "dnd", idleSince: null });
  });

  test("current() reports the live away+idle so a reconnect re-broadcasts it", () => {
    const stoppedAt = h.at();
    h.advance(AWAY_MS + 1);
    expect(h.tracker.current()).toEqual({ show: "away", idleSince: stoppedAt });
  });

  test("resuming Automatic after a manual status restarts the clock by itself", () => {
    // No markActive() here — the tracker must self-reset on the manual→auto
    // edge so a stale pre-manual idle instant never resurfaces.
    h.setMode({ kind: "manual", status: "dnd" });
    h.tracker.evaluate(); // suspend
    h.advance(XA_MS + 1); // long idle accrues while suspended
    h.setMode({ kind: "automatic" });
    h.tracker.evaluate();
    // Resumes as Available (clock restarted), not a stale Extended Away.
    expect(h.tracker.current().show).toBe("available");
    expect(h.sent).toEqual([]);
  });
});

// The DOM half of the state machine — start() wiring interaction + visibility
// to markActive. Exercises the acceptance criterion "any interaction or tab
// refocus restores Available promptly" and the hidden-runs-the-clock model.
describe("IdleTracker DOM wiring", () => {
  const originalWindow = globalThis.window;
  const originalDocument = globalThis.document;
  let clock: number;
  let win: EventTarget;
  let doc: EventTarget & { visibilityState: string };

  function set(name: string, value: unknown) {
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  }

  beforeEach(() => {
    clock = 1_000_000;
    win = new EventTarget();
    doc = Object.assign(new EventTarget(), { visibilityState: "visible" });
    set("window", win);
    set("document", doc);
  });

  afterEach(() => {
    set("window", originalWindow);
    set("document", originalDocument);
  });

  function startedTracker() {
    const sent: BroadcastPresence[] = [];
    const tracker = new IdleTracker({
      now: () => clock,
      getMode: () => ({ kind: "automatic" }),
      onBroadcast: (b) => sent.push(b),
      thresholds,
    });
    tracker.start();
    return { tracker, sent };
  }

  test("a window focus event restores Available after going idle", () => {
    const { tracker, sent } = startedTracker();
    clock += AWAY_MS + 1;
    tracker.evaluate(); // away
    win.dispatchEvent(new Event("focus"));
    tracker.stop();
    expect(sent.at(-1)).toEqual({ show: "available", idleSince: null });
  });

  test("becoming visible restores Available; a hidden tab keeps the clock running", () => {
    const { tracker, sent } = startedTracker();
    clock += AWAY_MS + 1;
    tracker.evaluate(); // away
    // visibilitychange while hidden must NOT reset the clock.
    doc.visibilityState = "hidden";
    doc.dispatchEvent(new Event("visibilitychange"));
    expect(sent.at(-1)?.show).toBe("away");
    // Returning to visible restores Available.
    doc.visibilityState = "visible";
    doc.dispatchEvent(new Event("visibilitychange"));
    tracker.stop();
    expect(sent.at(-1)).toEqual({ show: "available", idleSince: null });
  });

  test("stop() unwires the listeners so later events are inert", () => {
    const { tracker, sent } = startedTracker();
    clock += AWAY_MS + 1;
    tracker.evaluate(); // away
    tracker.stop();
    win.dispatchEvent(new Event("focus"));
    // No restore broadcast after stop().
    expect(sent.at(-1)?.show).toBe("away");
  });
});
