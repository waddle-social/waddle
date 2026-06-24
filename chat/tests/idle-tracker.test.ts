import { beforeEach, describe, expect, test } from "bun:test";

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
});
