/**
 * One bookkeeping record per `(tile, role)` slot. Tracks which
 * (`<video>` | `<audio>`, LiveKit track) pair is currently mounted so
 * Vue's `:ref` lifecycle can correctly detach the previous track
 * when either side changes — without this, LiveKit's
 * `attachedElements` array would grow on every focus toggle and the
 * same track would render into multiple media elements, each
 * fighting for the same MediaStream tap.
 */
type TileAttachKey = string;
export type TileAttachable = {
  attach: (target: HTMLMediaElement) => void;
  detach: (target: HTMLMediaElement) => HTMLMediaElement;
};
type AttachRecord = {
  el: HTMLMediaElement;
  track: TileAttachable;
};

/**
 * Map-backed attachment manager. One instance per CallTileGrid
 * (component-scoped). Vue's `:ref` callback wires (element, track)
 * pairs in/out through `sync`; the manager handles the previous-record
 * detach + srcObject release on every change.
 */
export class TileAttachments {
  private records = new Map<TileAttachKey, AttachRecord>();

  /**
   * Reconcile the (element, track) pair for the given slot.
   *
   * Vue invokes our `:ref` callback in three shapes:
   *   1. mount   → `el` is the live element, `track` is the current track.
   *   2. unmount → `el` is `null` (Vue's signal that the element left
   *      the DOM); `track` may be either value.
   *   3. update  → both `el` and `track` may stay or change.
   *
   * The contract: at most one (el, track) pair is live per `key`, and
   * the previous track is always `.detach()`-ed before the next is
   * attached so LiveKit's internal `attachedElements` never accumulates
   * stale entries.
   */
  sync(
    key: TileAttachKey,
    el: HTMLMediaElement | null,
    track: TileAttachable | null,
  ): void {
    const prev = this.records.get(key);
    // Unmount path: free the previous attachment and clear the
    // srcObject so the browser can release the underlying
    // MediaStreamTrack tap.
    if (!el || !track) {
      if (prev) {
        try {
          prev.track.detach(prev.el);
        } catch {
          // detach is idempotent in normal flow; swallow.
        }
        prev.el.srcObject = null;
        this.records.delete(key);
      }
      return;
    }
    // No-op: same (el, track) reattached. LiveKit tolerates this but
    // logs about it; skipping keeps the console clean.
    if (prev && prev.el === el && prev.track === track) return;
    // Either el or track changed → detach the previous record first.
    if (prev) {
      try {
        prev.track.detach(prev.el);
      } catch {
        // see above
      }
      prev.el.srcObject = null;
    }
    track.attach(el);
    this.records.set(key, { el, track });
  }

  /** Test helper: peek at the live records. */
  size(): number {
    return this.records.size;
  }

  detachAll(): void {
    for (const record of this.records.values()) {
      try {
        record.track.detach(record.el);
      } catch {
        // detach is idempotent in normal flow; swallow.
      }
      record.el.srcObject = null;
    }
    this.records.clear();
  }
}
