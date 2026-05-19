import type { InjectionKey } from "vue";

/**
 * Shared registry of `<video>` elements currently attached to call
 * tile tracks, keyed by participant identity. Populated by
 * `CallTileGrid` via `:ref` callbacks and read by `CallOverlay.togglePip`
 * so the overlay no longer has to reach into LiveKit's private
 * `attachedElements` array on a `RemoteTrack` / `LocalTrack`.
 *
 * Per-identity keys (not per-publication) so the lookup survives
 * track re-publishes (camera off → on) without the registry
 * accumulating stale entries — `register(identity, el)` overwrites
 * the previous element atomically.
 */
export type CallVideoRegistry = {
  register(identity: string, el: HTMLVideoElement | null): void;
  /**
   * Return a still-mounted video element for the given identity, or
   * `null` if nothing has been registered. Used by the overlay's
   * picture-in-picture toggle to find a target without crossing the
   * LiveKit SDK private surface.
   */
  get(identity: string): HTMLVideoElement | null;
  /**
   * Iterate registered (identity, element) pairs. The order is
   * insertion order — callers (e.g. `togglePip`) typically prefer the
   * first non-self entry, then fall back to any entry.
   */
  entries(): IterableIterator<[string, HTMLVideoElement]>;
};

/**
 * `provide`/`inject` key for the registry. CallOverlay constructs the
 * registry and provides it; CallTileGrid resolves it during render and
 * wires `<video>` element refs through `register`.
 */
export const callVideoRegistryKey = Symbol("callVideoRegistry") as InjectionKey<CallVideoRegistry>;

/**
 * Build a fresh registry backed by a plain Map. The CallOverlay owns
 * one instance per call lifetime; entries are added/removed by Vue's
 * `:ref` lifecycle (element on mount, `null` on unmount).
 */
export function createCallVideoRegistry(): CallVideoRegistry {
  const elements = new Map<string, HTMLVideoElement>();
  return {
    register(identity, el) {
      if (el) {
        elements.set(identity, el);
      } else {
        elements.delete(identity);
      }
    },
    get(identity) {
      return elements.get(identity) ?? null;
    },
    entries() {
      return elements.entries();
    },
  };
}
