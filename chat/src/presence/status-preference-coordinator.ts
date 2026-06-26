// Sole publisher of the user's status-preference PEP node (ADR-010 Phase 4).
// The node holds one item (`current`); this device's picked `PresenceMode` is
// the intent, and the coordinator keeps the node in sync with it while avoiding
// the obvious failure modes of two-way sync:
//
//   - Echo loop: adopting a pick that arrived from another resource must NOT
//     re-publish it (or two devices ping-pong forever). `applyRemote` seeds the
//     baseline before writing the store so the store-driven reconcile no-ops.
//   - Fresh-login clobber: a new device defaults to Automatic; it must adopt
//     the server's stored pick rather than publish its default over a peer's
//     manual status. `syncOnConnect` fetches and adopts unless this device has
//     an unpublished local pick.
//   - Logout clobber: signing out is local-only (a manual status must follow
//     the user to their other devices), so `suspend` stops the logout-time
//     reset from publishing Automatic.
//
// Diff-driven like ActivityCoordinator: writes report whether they reached the
// wire, so a pick made while disconnected is retried on the next reconcile.

import type { PresenceMode } from "./effective-show";

/** Structural equality for two presence modes (null = "unknown baseline"). */
export function sameMode(a: PresenceMode | null, b: PresenceMode | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind !== b.kind) return false;
  return a.kind === "manual" && b.kind === "manual" ? a.status === b.status : true;
}

export interface StatusPreferenceCoordinatorDeps {
  /** Publish the mode to the PEP node; returns whether it reached the wire. */
  publish: (mode: PresenceMode) => boolean;
  /** This device's current picked mode (reads `$presenceMode`). */
  mode: () => PresenceMode;
  /** Adopt a mode locally (writes `$presenceMode`). */
  setMode: (mode: PresenceMode) => void;
}

export class StatusPreferenceCoordinator {
  /** Last mode we know is on the wire; `null` until the first sync/publish. */
  private lastPublished: PresenceMode | null = null;
  private suspended = false;

  constructor(private readonly deps: StatusPreferenceCoordinatorDeps) {}

  /**
   * Publish the current mode if it differs from the wire baseline. Idempotent;
   * only advances the baseline when the write was actually sent, so a pick made
   * while disconnected is retried on the next reconcile (session-ready).
   */
  reconcile(): void {
    if (this.suspended) return;
    const desired = this.deps.mode();
    if (this.lastPublished !== null && sameMode(desired, this.lastPublished)) return;
    if (this.deps.publish(desired)) this.lastPublished = desired;
  }

  /**
   * Adopt a mode that arrived from another of the user's resources, WITHOUT
   * re-publishing it. The baseline is seeded first so the store write's
   * reconcile is a no-op — otherwise the two devices would echo forever.
   */
  applyRemote(mode: PresenceMode): void {
    this.lastPublished = mode;
    this.deps.setMode(mode);
  }

  /** True when this device holds a pick that has not reached the wire yet. */
  hasPendingLocalPick(): boolean {
    return this.lastPublished !== null && !sameMode(this.deps.mode(), this.lastPublished);
  }

  /**
   * Reconcile the local state against the node on (re)connect. An unpublished
   * local pick (e.g. chosen while disconnected) wins and is published;
   * otherwise the server's stored preference is adopted — a fresh device thus
   * inherits the user's pick rather than overwriting it with its default.
   */
  async syncOnConnect(fetch: () => Promise<PresenceMode | null>): Promise<void> {
    this.resume();
    if (this.hasPendingLocalPick()) {
      this.reconcile();
      return;
    }
    this.hydrate(await fetch());
  }

  /**
   * Apply a fetched preference. A stored pick is adopted. When nothing is
   * stored, a deliberate local manual pick is published, but the default
   * Automatic is treated as already-synced — publishing it would clobber a
   * peer's stored manual status on the peer's own next connect.
   */
  hydrate(fetched: PresenceMode | null): void {
    if (fetched) {
      this.applyRemote(fetched);
      return;
    }
    if (this.deps.mode().kind === "manual") {
      this.lastPublished = null;
      this.reconcile();
    } else {
      this.lastPublished = { kind: "automatic" };
    }
  }

  /**
   * Stop publishing and forget the baseline (logout). Signing out is local; the
   * cross-device preference must persist, so the local reset-to-Automatic that
   * follows must not publish. The next session re-syncs from a clean baseline.
   */
  suspend(): void {
    this.suspended = true;
    this.lastPublished = null;
  }

  /** Re-enable publishing (called on session-ready before fetch/hydrate). */
  resume(): void {
    this.suspended = false;
  }
}
