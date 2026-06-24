// Per-device auto-away driver (ADR-010 Phase 2). Watches in-tab interaction
// (mouse / keyboard / touch) and tab visibility to maintain a "last active"
// instant, then broadcasts the auto-away Show — Available → Away → Extended
// Away — stamped with XEP-0319 idle, and restores Available on any activity.
//
// Logic is injectable (clock, mode, broadcast sink) so the emission state
// machine is testable without a DOM; `start()`/`stop()` are the thin shell
// that wires real browser events and a poll timer to `markActive`/`evaluate`.

import {
  resolveBroadcast,
  type BroadcastPresence,
  type PresenceMode,
} from "./effective-show";
import { computeAutoAway, DEFAULT_AUTO_AWAY_THRESHOLDS, type AutoAwayThresholds } from "./idle";

export interface IdleTrackerDeps {
  /** Epoch-ms clock; injectable for tests. */
  now: () => number;
  /** This device's current presence mode (manual suspends auto-away). */
  getMode: () => PresenceMode;
  /** Called when the device's outbound presence should change. */
  onBroadcast: (presence: BroadcastPresence) => void;
  /** Inactivity thresholds; defaults to ADR-010's 10 min / 30 min. */
  thresholds?: AutoAwayThresholds;
  /** How often to re-check the idle clock while running. Default 30 s. */
  pollMs?: number;
}

const AVAILABLE: BroadcastPresence = { show: "available", idleSince: null };

const INTERACTION_EVENTS = ["mousedown", "mousemove", "keydown", "touchstart", "wheel", "scroll"];

function samePresence(a: BroadcastPresence, b: BroadcastPresence): boolean {
  return a.show === b.show && a.idleSince === b.idleSince;
}

export class IdleTracker {
  private readonly deps: IdleTrackerDeps;
  private readonly thresholds: AutoAwayThresholds;
  private lastActive: number;
  // Baseline the wire is assumed to already hold (the controller broadcasts
  // Available on connect), so the tracker stays silent until it diverges.
  private lastEmitted: BroadcastPresence = AVAILABLE;
  private timer: ReturnType<typeof setInterval> | null = null;
  private readonly boundMarkActive = () => this.markActive();
  private readonly boundVisibility = () => {
    // Becoming visible/focused is itself a "the user is back" signal; while
    // hidden we simply stop resetting the clock so it runs toward the threshold.
    if (typeof document === "undefined" || document.visibilityState === "visible") {
      this.markActive();
    }
  };

  constructor(deps: IdleTrackerDeps) {
    this.deps = deps;
    this.thresholds = deps.thresholds ?? DEFAULT_AUTO_AWAY_THRESHOLDS;
    this.lastActive = deps.now();
  }

  /** The outbound presence implied by the mode + idle clock right now. */
  current(): BroadcastPresence {
    const autoAway = computeAutoAway({
      now: this.deps.now(),
      lastActive: this.lastActive,
      ...this.thresholds,
    });
    return resolveBroadcast(this.deps.getMode(), autoAway);
  }

  /** The user interacted or the tab refocused: reset the clock, restore promptly. */
  markActive(): void {
    this.lastActive = this.deps.now();
    this.evaluate();
  }

  /** Re-derive the outbound presence; broadcast only on a real change. */
  evaluate(): void {
    if (this.deps.getMode().kind === "manual") {
      // A manual status suspends auto-away — the picker owns the wire. Reset
      // the baseline so returning to Automatic re-emits the right Show.
      this.lastEmitted = AVAILABLE;
      return;
    }
    const next = this.current();
    if (samePresence(next, this.lastEmitted)) return;
    this.lastEmitted = next;
    this.deps.onBroadcast(next);
  }

  /** Wire real interaction + visibility listeners and the poll timer. */
  start(): void {
    if (typeof window === "undefined" || typeof document === "undefined") return;
    for (const event of INTERACTION_EVENTS) {
      window.addEventListener(event, this.boundMarkActive, { passive: true });
    }
    document.addEventListener("visibilitychange", this.boundVisibility);
    window.addEventListener("focus", this.boundMarkActive);
    this.timer = setInterval(() => this.evaluate(), this.deps.pollMs ?? 30_000);
  }

  stop(): void {
    if (typeof window === "undefined" || typeof document === "undefined") return;
    for (const event of INTERACTION_EVENTS) {
      window.removeEventListener(event, this.boundMarkActive);
    }
    document.removeEventListener("visibilitychange", this.boundVisibility);
    window.removeEventListener("focus", this.boundMarkActive);
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }
}
