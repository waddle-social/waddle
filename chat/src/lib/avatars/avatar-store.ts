/**
 * JID-keyed peer avatar store — the single source of truth for every web
 * surface that renders a person's XEP-0084 avatar.
 *
 * Refresh policy (shared with the native apps):
 * - Keyed by bare JID only, never by nick.
 * - Lazy: a JID is fetched the first time a surface retains it.
 * - One fetch per JID at a time; at most {@link MAX_CONCURRENT_FETCHES}
 *   fetches in flight, the rest queue.
 * - A positive result is revalidated after {@link POSITIVE_TTL_MS}; a
 *   negative or failed result is retried after {@link NEGATIVE_TTL_MS}.
 * - A new session (reconnect) marks every entry stale.
 * - A peer's avatar-change notification refetches that JID; a disable
 *   (no avatar id) clears it to initials immediately.
 */
import { shallowReactive } from "vue";
import { barePeerJid } from "@/lib/xmpp/jid";

export const POSITIVE_TTL_MS = 45 * 60_000;
export const NEGATIVE_TTL_MS = 10 * 60_000;
export const MAX_CONCURRENT_FETCHES = 4;

/** Resolves a bare JID to a displayable image URL, or `null` for none. */
export type AvatarFetcher = (bareJid: string) => Promise<string | null>;

type TimerHandle = ReturnType<typeof setTimeout>;

export interface AvatarStoreClock {
  now: () => number;
  setTimer: (callback: () => void, delayMs: number) => TimerHandle;
  clearTimer: (handle: TimerHandle) => void;
}

interface Settled {
  /** `ok` = the peer has an avatar; `miss` = none or the fetch failed. */
  kind: "ok" | "miss";
  settledAt: number;
  stale: boolean;
}

interface Entry {
  settled: Settled | null;
  retainers: number;
  /** Bumped by every invalidation; a fetch result from an older epoch is dropped. */
  epoch: number;
  inFlight: boolean;
  queued: boolean;
  timer: TimerHandle | null;
}

const systemClock: AvatarStoreClock = {
  now: () => Date.now(),
  setTimer: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimer: (handle) => clearTimeout(handle),
};

function avatarKey(jid: string): string {
  return barePeerJid(jid.trim()).toLowerCase();
}

export class AvatarStore {
  /** Bare JID → image URL, or `null` once the peer is known to have no avatar. */
  private readonly urls = shallowReactive(new Map<string, string | null>());
  private readonly entries = new Map<string, Entry>();
  private readonly queue: string[] = [];
  private inFlightCount = 0;
  private fetcher: AvatarFetcher | null = null;
  private hadSession = false;

  constructor(private readonly clock: AvatarStoreClock = systemClock) {}

  /** Reactive: the current image URL for `jid`, or `null` (render initials). */
  urlFor(jid: string | null | undefined): string | null {
    if (!jid) return null;
    return this.urls.get(avatarKey(jid)) ?? null;
  }

  /**
   * Reactive: whether `jid` is known to have no avatar (a definitive
   * "none" answer or a disable), as opposed to not yet resolved.
   */
  isKnownAbsent(jid: string | null | undefined): boolean {
    if (!jid) return false;
    const key = avatarKey(jid);
    return this.urls.has(key) && this.urls.get(key) === null;
  }

  /**
   * Declare that a surface is showing `jid`. Fetches lazily when the JID
   * has no fresh result, and keeps it revalidating while retained.
   * Returns the release function.
   */
  retain(jid: string): () => void {
    const key = avatarKey(jid);
    if (!key) return () => undefined;
    const entry = this.entry(key);
    entry.retainers += 1;
    if (!this.isFresh(entry)) this.enqueue(key);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      entry.retainers = Math.max(0, entry.retainers - 1);
    };
  }

  /** Bind (or unbind with `null`) the transport that performs fetches. */
  setFetcher(fetcher: AvatarFetcher | null): void {
    this.fetcher = fetcher;
    if (fetcher) this.pump();
  }

  /**
   * A new (non-resumed) XMPP session is ready. The first one only starts
   * the store; every later one is a reconnect, so all results go stale.
   */
  beginSession(): void {
    if (this.hadSession) this.markAllStale();
    this.hadSession = true;
  }

  /** Nothing cached can be trusted as current. */
  markAllStale(): void {
    for (const [key, entry] of this.entries) {
      if (entry.settled) entry.settled.stale = true;
      if (entry.retainers > 0) this.enqueue(key, true);
    }
  }

  /** XEP-0084 metadata transition for `jid`; `avatarId` absent = disabled. */
  handleAvatarChanged(jid: string, avatarId?: string): void {
    const key = avatarKey(jid);
    if (!key) return;
    if (avatarId) {
      this.invalidate(key);
      return;
    }
    const entry = this.entry(key);
    entry.epoch += 1;
    this.urls.set(key, null);
    this.settle(key, entry, "miss");
  }

  /** Force a refetch of `jid` (e.g. after the user republished their own profile). */
  invalidate(jid: string): void {
    const key = avatarKey(jid);
    if (!key) return;
    const entry = this.entry(key);
    entry.epoch += 1;
    if (entry.settled) entry.settled.stale = true;
    if (entry.retainers > 0 || entry.settled) this.enqueue(key, true);
  }

  /** Forget everything (logout). */
  reset(): void {
    for (const entry of this.entries.values()) {
      if (entry.timer) this.clock.clearTimer(entry.timer);
    }
    this.entries.clear();
    this.queue.length = 0;
    this.urls.clear();
    this.inFlightCount = 0;
    this.fetcher = null;
    this.hadSession = false;
  }

  private entry(key: string): Entry {
    let entry = this.entries.get(key);
    if (!entry) {
      entry = { settled: null, retainers: 0, epoch: 0, inFlight: false, queued: false, timer: null };
      this.entries.set(key, entry);
    }
    return entry;
  }

  private isFresh(entry: Entry): boolean {
    const settled = entry.settled;
    if (!settled || settled.stale) return false;
    const ttl = settled.kind === "ok" ? POSITIVE_TTL_MS : NEGATIVE_TTL_MS;
    return this.clock.now() - settled.settledAt < ttl;
  }

  /**
   * Queue a fetch. A JID already in flight is only queued again when
   * `afterInFlight` says the in-flight answer may be outdated.
   */
  private enqueue(key: string, afterInFlight = false): void {
    const entry = this.entry(key);
    if (entry.queued || (entry.inFlight && !afterInFlight)) return;
    entry.queued = true;
    this.queue.push(key);
    this.pump();
  }

  private pump(): void {
    const fetcher = this.fetcher;
    if (!fetcher) return;
    let index = 0;
    while (this.inFlightCount < MAX_CONCURRENT_FETCHES && index < this.queue.length) {
      const key = this.queue[index]!;
      const entry = this.entry(key);
      // A JID already being fetched stays queued until that fetch settles.
      if (entry.inFlight) {
        index += 1;
        continue;
      }
      this.queue.splice(index, 1);
      entry.queued = false;
      void this.run(key, entry, fetcher);
    }
  }

  private async run(key: string, entry: Entry, fetcher: AvatarFetcher): Promise<void> {
    entry.inFlight = true;
    this.inFlightCount += 1;
    const epoch = entry.epoch;
    let url: string | null = null;
    let failed = false;
    try {
      url = await fetcher(key);
    } catch {
      failed = true;
    }
    // `reset()` replaced the entry map: this result belongs to a dead session.
    if (this.entries.get(key) !== entry) return;
    entry.inFlight = false;
    this.inFlightCount -= 1;
    if (entry.epoch === epoch) {
      if (url) {
        this.urls.set(key, url);
        this.settle(key, entry, "ok");
      } else {
        // A transport failure keeps the last known face; a definitive
        // "no avatar" clears it. Both retry on the negative TTL.
        if (!failed) this.urls.set(key, null);
        this.settle(key, entry, "miss");
      }
    }
    this.pump();
  }

  private settle(key: string, entry: Entry, kind: Settled["kind"]): void {
    entry.settled = { kind, settledAt: this.clock.now(), stale: false };
    if (entry.timer) this.clock.clearTimer(entry.timer);
    const ttl = kind === "ok" ? POSITIVE_TTL_MS : NEGATIVE_TTL_MS;
    entry.timer = this.clock.setTimer(() => {
      entry.timer = null;
      if (entry.retainers > 0 && !this.isFresh(entry)) this.enqueue(key);
    }, ttl);
  }
}

/** Process-wide store every avatar surface reads. */
export const avatarStore = new AvatarStore();
