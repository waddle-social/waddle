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
//     a pending local pick.
//   - Disconnected-pick loss: a manual pick made before any successful publish
//     (offline, or before the first session) must win on connect, not be
//     overwritten by the stale stored value — see `hasPendingLocalPick`.
//   - Logout clobber: signing out is local-only (a manual status must follow
//     the user to their other devices), so `suspend` stops the logout-time
//     reset from publishing Automatic.
//
// The baseline (`lastPublished`) only advances when a publish actually reaches
// the wire: `publish` resolves `true` iff the IQ was sent, and `reconcile`
// awaits it. A publish that fails (disconnected, session not ready, server
// error) leaves the baseline untouched so the next reconcile retries.
//
// Because publishing is async, the baseline can be changed *while a publish is
// in flight*. An `epoch` counter disowns a publish whose baseline was moved
// under it (by `suspend` or `applyRemote`), so its late success can neither
// echo an adopted remote pick nor resurrect a pre-logout one. A trigger that
// fires mid-publish is coalesced (`rerunRequested`) rather than dropped, so a
// pick made during a publish is honored even if that publish then fails.

import type { PresenceMode } from "./effective-show";

/** Structural equality for two presence modes (null = "unknown baseline"). */
export function sameMode(a: PresenceMode | null, b: PresenceMode | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind !== b.kind) return false;
  return a.kind === "manual" && b.kind === "manual" ? a.status === b.status : true;
}

export interface StatusPreferenceCoordinatorDeps {
  /**
   * Publish the mode to the PEP node. Resolves `true` IFF it reached the wire;
   * resolves `false` on any failure (disconnected, session not ready, server
   * error) so the baseline is not advanced and the next reconcile retries.
   */
  publish: (mode: PresenceMode) => Promise<boolean>;
  /** This device's current picked mode (reads `$presenceMode`). */
  mode: () => PresenceMode;
  /** Adopt a mode locally (writes `$presenceMode`). */
  setMode: (mode: PresenceMode) => void;
}

export class StatusPreferenceCoordinator {
  /** Last mode confirmed on the wire; `null` until the first success/hydrate. */
  private lastPublished: PresenceMode | null = null;
  private suspended = false;
  /** Guards against overlapping publishes (an async publish is in flight). */
  private publishing = false;
  /** A reconcile trigger that arrived mid-publish, to be honored once on settle. */
  private rerunRequested = false;
  /**
   * Bumped whenever the baseline is changed out from under an in-flight publish
   * — by logout (`suspend`) or by adopting a remote pick (`applyRemote`). A
   * publish that resolves after the epoch moved no longer owns the baseline and
   * must not advance it (which would echo an adopted pick or resurrect a
   * pre-logout one). Robust to a `resume()` that flips `suspended` back.
   */
  private epoch = 0;

  constructor(private readonly deps: StatusPreferenceCoordinatorDeps) {}

  /**
   * Publish the current mode if it differs from the wire baseline, advancing
   * the baseline only when the publish is confirmed sent. A publish that fails
   * leaves the baseline untouched, so the next reconcile retries. A trigger
   * arriving mid-publish is coalesced into one rerun (never dropped), so a pick
   * made while a publish is in flight is honored even if that publish fails. A
   * baseline moved mid-flight by `suspend`/`applyRemote` is left untouched.
   */
  async reconcile(): Promise<void> {
    if (this.suspended) return;
    // Coalesce, don't drop: a pick that fires while a publish is in flight must
    // still be published once the current one settles — including when it fails
    // (the success-only recursion below would otherwise strand it).
    if (this.publishing) {
      this.rerunRequested = true;
      return;
    }
    const desired = this.deps.mode();
    if (this.lastPublished !== null && sameMode(desired, this.lastPublished)) return;
    this.publishing = true;
    const startEpoch = this.epoch;
    let sent = false;
    try {
      sent = await this.deps.publish(desired);
    } finally {
      this.publishing = false;
    }
    // The baseline moved under us while the publish was in flight: logout
    // (`suspend`) zeroed it, or a remote pick was adopted (`applyRemote`).
    // Advancing it now would resurrect a stale value — re-publishing an adopted
    // remote pick (echo), or clobbering the synced preference after logout.
    // Leave the baseline as the newer writer left it. But a trigger that landed
    // mid-flight (e.g. a local pick made after the adopted remote) must still
    // reconcile against the NEW baseline — recursing is safe, it re-reads the
    // mode and no-ops if it already matches (and bails if suspended).
    if (this.epoch !== startEpoch) {
      const rerun = this.rerunRequested;
      this.rerunRequested = false;
      if (rerun) await this.reconcile();
      return;
    }
    if (sent) this.lastPublished = desired;
    // Converge once more if a trigger landed during the flight (coalesced
    // rerun) or a successful publish still trails the current mode. Only a real
    // external trigger re-arms `rerunRequested`, so a persistent failure retries
    // the stranded pick exactly once and then waits for the next trigger — no
    // hot loop.
    const rerun = this.rerunRequested || (sent && !sameMode(this.deps.mode(), this.lastPublished));
    this.rerunRequested = false;
    if (rerun) await this.reconcile();
  }

  /**
   * Adopt a mode that arrived from another of the user's resources, WITHOUT
   * re-publishing it. The baseline is seeded first so the store write's
   * reconcile is a no-op — otherwise the two devices would echo forever. The
   * epoch bump disowns any in-flight publish so its late success can't clobber
   * this adopted baseline back onto the wire.
   */
  applyRemote(mode: PresenceMode): void {
    if (this.suspended) return; // a post-logout event must not revive the store
    this.epoch += 1;
    this.lastPublished = mode;
    this.deps.setMode(mode);
  }

  /**
   * Whether this device holds a pick that should be published rather than
   * overwritten by the stored value on connect. With no confirmed baseline yet
   * (`lastPublished === null`), a deliberate Manual pick counts as pending
   * intent — so a status chosen while disconnected wins over the stale stored
   * value — but the default Automatic does not (a fresh device must adopt a
   * peer's stored pick, not clobber it with its default). With a baseline, any
   * divergence from it is a pending pick.
   */
  hasPendingLocalPick(): boolean {
    const mode = this.deps.mode();
    if (this.lastPublished === null) return mode.kind === "manual";
    return !sameMode(mode, this.lastPublished);
  }

  /**
   * Reconcile the local state against the node on (re)connect. A pending local
   * pick (offline pick, or a pick made before the first successful publish)
   * wins and is published; otherwise the server's stored preference is adopted
   * — a fresh device thus inherits the user's pick rather than overwriting it
   * with its default. When nothing is stored and there is no pending pick, the
   * current (Automatic) state is the synced baseline.
   */
  async syncOnConnect(fetch: () => Promise<PresenceMode | null>): Promise<void> {
    this.resume();
    if (this.hasPendingLocalPick()) {
      await this.reconcile();
      return;
    }
    const before = this.deps.mode();
    const fetched = await fetch();
    // Logout may have raced the fetch (suspend() + reset-to-Automatic ran while
    // the IQ was in flight). Drop the stored value rather than write it into the
    // store after the reset — it would otherwise leak the signed-out user's pick
    // into the next account opened in this tab.
    if (this.suspended) return;
    // A newer intent may have landed while the fetch was in flight: a local
    // pick (the `$presenceMode` subscriber fired `reconcile()`) or an inbound
    // peer update (`applyRemote`). Either is fresher than the snapshot we just
    // fetched, so adopting `fetched` here would overwrite it. If the mode
    // changed, reconcile instead — it publishes a local pick and no-ops an
    // already-applied remote one, never clobbering the newer value.
    if (!sameMode(before, this.deps.mode())) {
      await this.reconcile();
      return;
    }
    if (fetched) {
      this.applyRemote(fetched);
    } else {
      this.lastPublished = this.deps.mode();
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
    this.epoch += 1; // disown any in-flight publish so its late success can't re-advance the baseline
  }

  /** Re-enable publishing (called on session-ready before fetch/sync). */
  resume(): void {
    this.suspended = false;
  }
}
