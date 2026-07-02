/**
 * Typed event bus backing `BrowserXmppClient`'s handler-registration API.
 *
 * The client historically kept ~25 nullable single-consumer handler fields
 * (`setXHandler(fn)` replaces the previous handler) plus telemetry hook
 * arrays (`onX(fn)` appends). This bus preserves both semantics:
 *
 * - `set(event, fn)`  — single-listener: replaces every listener for the
 *   event (mirrors `this.xHandler = fn`; `null` clears, mirroring
 *   `setMdsDisplayedHandler(null)` / `setPubsubEventHandler(null)`).
 * - `on(event, fn)`   — multi-listener: appends and returns an unsubscribe
 *   (mirrors `this.xHooks.push(fn)` / `pubsubEventHandlers.add`).
 * - `emit(event, …)`  — invokes listeners without error isolation, exactly
 *   like the old `this.xHandler?.(…)` calls and the pubsub fan-out loop:
 *   a throwing listener propagates to the emitter.
 * - `emitSafe(event, …)` — per-listener try/catch, mirroring the old
 *   `fireHook` used for observe-only telemetry hooks: one throwing hook
 *   must not break the others or the client.
 */

/** Event name → payload tuple. */
export type ClientEventMap = Record<string, ReadonlyArray<unknown>>;

type GenericListener = (...args: ReadonlyArray<unknown>) => void;

export class TypedEventBus<Events extends ClientEventMap> {
  private readonly listeners = new Map<keyof Events, Set<GenericListener>>();

  /** Append a listener. Returns an unsubscribe function. */
  on<K extends keyof Events>(event: K, listener: (...args: Events[K]) => void): () => void {
    let set = this.listeners.get(event);
    if (!set) {
      set = new Set();
      this.listeners.set(event, set);
    }
    const generic = listener as GenericListener;
    set.add(generic);
    return () => {
      this.listeners.get(event)?.delete(generic);
    };
  }

  /**
   * Single-listener registration: replaces every listener for `event`
   * (matching the old `this.xHandler = fn` setter semantics). Passing
   * `null` clears the event's listeners.
   */
  set<K extends keyof Events>(event: K, listener: ((...args: Events[K]) => void) | null): void {
    if (!listener) {
      this.listeners.delete(event);
      return;
    }
    this.listeners.set(event, new Set([listener as GenericListener]));
  }

  /**
   * Invoke listeners without error isolation — a throwing listener
   * propagates, exactly like the old direct `this.xHandler?.(…)` calls.
   */
  emit<K extends keyof Events>(event: K, ...args: Events[K]): void {
    const set = this.listeners.get(event);
    if (!set || set.size === 0) return;
    for (const listener of [...set]) {
      listener(...args);
    }
  }

  /**
   * Invoke listeners with per-listener error isolation — mirrors the old
   * `fireHook` behavior for observe-only telemetry hooks.
   */
  emitSafe<K extends keyof Events>(event: K, ...args: Events[K]): void {
    const set = this.listeners.get(event);
    if (!set || set.size === 0) return;
    for (const listener of [...set]) {
      try {
        listener(...args);
      } catch (error) {
        console.error("xmpp telemetry hook threw", error);
      }
    }
  }
}
