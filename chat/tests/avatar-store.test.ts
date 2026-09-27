import { describe, expect, test } from "bun:test";
import { computed } from "vue";
import {
  AvatarStore,
  MAX_CONCURRENT_FETCHES,
  NEGATIVE_TTL_MS,
  POSITIVE_TTL_MS,
  type AvatarStoreClock,
} from "../src/lib/avatars/avatar-store";

type TimerHandle = ReturnType<AvatarStoreClock["setTimer"]>;

/** Manual clock: `advance` fires due timers in order. */
function fakeClock() {
  let now = 0;
  let nextId = 1;
  const timers = new Map<number, { at: number; callback: () => void }>();
  const clock: AvatarStoreClock = {
    now: () => now,
    setTimer: (callback, delayMs) => {
      const id = nextId++;
      timers.set(id, { at: now + delayMs, callback });
      return id as unknown as TimerHandle;
    },
    clearTimer: (handle) => {
      timers.delete(handle as unknown as number);
    },
  };
  return {
    clock,
    advance(ms: number) {
      const target = now + ms;
      for (;;) {
        const due = [...timers.entries()]
          .filter(([, timer]) => timer.at <= target)
          .sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        timers.delete(due[0]);
        now = due[1].at;
        due[1].callback();
      }
      now = target;
    },
  };
}

/** Fetcher whose calls resolve only when the test says so. */
function controlledFetcher() {
  const calls: Array<{ jid: string; resolve: (url: string | null) => void; reject: (error: Error) => void }> = [];
  const fetcher = (jid: string) =>
    new Promise<string | null>((resolve, reject) => {
      calls.push({ jid, resolve, reject });
    });
  return { calls, fetcher };
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function setup() {
  const time = fakeClock();
  const store = new AvatarStore(time.clock);
  const remote = controlledFetcher();
  store.setFetcher(remote.fetcher);
  return { store, time, remote };
}

describe("AvatarStore", () => {
  test("fetches lazily on first retain, keyed by bare JID", async () => {
    const { store, remote } = setup();
    expect(remote.calls).toHaveLength(0);
    expect(store.urlFor("alice@example.com")).toBeNull();

    store.retain("Alice@Example.com/phone");
    expect(remote.calls.map((call) => call.jid)).toEqual(["alice@example.com"]);
    remote.calls[0]!.resolve("data:image/png;base64,A");
    await flush();

    expect(store.urlFor("alice@example.com")).toBe("data:image/png;base64,A");
    expect(store.urlFor("alice@example.com/laptop")).toBe("data:image/png;base64,A");
  });

  test("urlFor is reactive", async () => {
    const { store, remote } = setup();
    const url = computed(() => store.urlFor("alice@example.com"));
    expect(url.value).toBeNull();
    store.retain("alice@example.com");
    remote.calls[0]!.resolve("data:a");
    await flush();
    expect(url.value).toBe("data:a");
  });

  test("dedupes concurrent requests for the same JID", async () => {
    const { store, remote } = setup();
    store.retain("alice@example.com");
    store.retain("alice@example.com");
    store.retain("alice@example.com/other");
    expect(remote.calls).toHaveLength(1);
    remote.calls[0]!.resolve("data:a");
    await flush();
    store.retain("alice@example.com");
    expect(remote.calls).toHaveLength(1);
  });

  test("caps concurrent fetches and drains the queue as fetches settle", async () => {
    const { store, remote } = setup();
    for (let i = 0; i < 6; i += 1) store.retain(`user${i}@example.com`);
    expect(remote.calls).toHaveLength(MAX_CONCURRENT_FETCHES);

    remote.calls[0]!.resolve(null);
    await flush();
    expect(remote.calls).toHaveLength(MAX_CONCURRENT_FETCHES + 1);
    remote.calls[1]!.resolve("data:1");
    await flush();
    expect(remote.calls.map((call) => call.jid)).toEqual([
      "user0@example.com",
      "user1@example.com",
      "user2@example.com",
      "user3@example.com",
      "user4@example.com",
      "user5@example.com",
    ]);
  });

  test("does not fetch before a transport is bound, then drains", async () => {
    const time = fakeClock();
    const store = new AvatarStore(time.clock);
    const remote = controlledFetcher();
    store.retain("alice@example.com");
    expect(remote.calls).toHaveLength(0);
    store.setFetcher(remote.fetcher);
    expect(remote.calls.map((call) => call.jid)).toEqual(["alice@example.com"]);
  });

  test("revalidates a positive result after 45 minutes while retained", async () => {
    const { store, time, remote } = setup();
    store.retain("alice@example.com");
    remote.calls[0]!.resolve("data:old");
    await flush();

    time.advance(POSITIVE_TTL_MS - 1);
    expect(remote.calls).toHaveLength(1);
    time.advance(1);
    expect(remote.calls).toHaveLength(2);
    // The old face stays up while revalidating.
    expect(store.urlFor("alice@example.com")).toBe("data:old");
    remote.calls[1]!.resolve("data:new");
    await flush();
    expect(store.urlFor("alice@example.com")).toBe("data:new");
  });

  test("an expired positive result refetches on the next retain once nobody watched it", async () => {
    const { store, time, remote } = setup();
    const release = store.retain("alice@example.com");
    remote.calls[0]!.resolve("data:a");
    await flush();
    release();

    time.advance(POSITIVE_TTL_MS);
    expect(remote.calls).toHaveLength(1);
    store.retain("alice@example.com");
    expect(remote.calls).toHaveLength(2);
  });

  test("retries a negative result after 10 minutes, not forever-cached", async () => {
    const { store, time, remote } = setup();
    store.retain("bob@example.com");
    remote.calls[0]!.resolve(null);
    await flush();
    expect(store.urlFor("bob@example.com")).toBeNull();

    store.retain("bob@example.com");
    expect(remote.calls).toHaveLength(1);
    time.advance(NEGATIVE_TTL_MS);
    expect(remote.calls).toHaveLength(2);
    remote.calls[1]!.resolve("data:b");
    await flush();
    expect(store.urlFor("bob@example.com")).toBe("data:b");
  });

  test("a failed fetch retries after 10 minutes and keeps the last known face", async () => {
    const { store, time, remote } = setup();
    store.retain("alice@example.com");
    remote.calls[0]!.resolve("data:a");
    await flush();

    time.advance(POSITIVE_TTL_MS);
    remote.calls[1]!.reject(new Error("timeout"));
    await flush();
    expect(store.urlFor("alice@example.com")).toBe("data:a");

    time.advance(NEGATIVE_TTL_MS);
    expect(remote.calls).toHaveLength(3);
  });

  test("a definitive no-avatar answer clears a previously known face", async () => {
    const { store, time, remote } = setup();
    store.retain("alice@example.com");
    remote.calls[0]!.resolve("data:a");
    await flush();
    time.advance(POSITIVE_TTL_MS);
    remote.calls[1]!.resolve(null);
    await flush();
    expect(store.urlFor("alice@example.com")).toBeNull();
  });

  test("a reconnect session marks everything stale: retained JIDs refetch, others on next retain", async () => {
    const { store, remote } = setup();
    store.beginSession();
    store.retain("alice@example.com");
    const releaseBob = store.retain("bob@example.com");
    remote.calls[0]!.resolve("data:a");
    remote.calls[1]!.resolve(null);
    await flush();
    releaseBob();

    store.beginSession();
    expect(remote.calls.map((call) => call.jid).slice(2)).toEqual(["alice@example.com"]);
    // Stale positive results keep rendering until the refetch answers.
    expect(store.urlFor("alice@example.com")).toBe("data:a");

    store.retain("bob@example.com");
    expect(remote.calls.map((call) => call.jid).slice(2)).toEqual(["alice@example.com", "bob@example.com"]);
  });

  test("the first session keeps positives but retries misses; later sessions mark everything stale", async () => {
    const { store, remote } = setup();
    store.retain("alice@example.com");
    store.retain("bob@example.com");
    remote.calls[0]!.resolve("data:a");
    // Recorded while the first connect was still failing.
    remote.calls[1]!.reject(new Error("not connected"));
    await flush();

    store.beginSession();
    expect(remote.calls.map((call) => call.jid).slice(2)).toEqual(["bob@example.com"]);
    remote.calls[2]!.resolve("data:b");
    await flush();
    expect(store.urlFor("bob@example.com")).toBe("data:b");

    store.beginSession();
    expect(remote.calls.map((call) => call.jid).slice(3)).toEqual(["alice@example.com", "bob@example.com"]);
  });

  test("an entry nobody retains is evicted when its timer fires, and the eviction hook runs", async () => {
    const { store, time, remote } = setup();
    const evicted: string[] = [];
    store.setEvictionHandler((jid) => evicted.push(jid));
    const releaseAlice = store.retain("alice@example.com");
    store.retain("bob@example.com");
    remote.calls[0]!.resolve("data:a");
    remote.calls[1]!.resolve(null);
    await flush();
    releaseAlice();

    time.advance(NEGATIVE_TTL_MS);
    // Bob is still shown: revalidated, not evicted.
    expect(evicted).toEqual([]);
    expect(remote.calls.map((call) => call.jid).slice(2)).toEqual(["bob@example.com"]);

    time.advance(POSITIVE_TTL_MS - NEGATIVE_TTL_MS);
    expect(evicted).toEqual(["alice@example.com"]);
    expect(store.urlFor("alice@example.com")).toBeNull();
    expect(store.isKnownAbsent("alice@example.com")).toBe(false);

    // Showing Alice again starts from scratch.
    store.retain("alice@example.com");
    expect(remote.calls.map((call) => call.jid).at(-1)).toBe("alice@example.com");
  });

  test("avatarChanged with an id refetches that JID", async () => {
    const { store, remote } = setup();
    store.retain("alice@example.com");
    remote.calls[0]!.resolve("data:a");
    await flush();

    store.handleAvatarChanged("alice@example.com", "sha1-new");
    expect(remote.calls).toHaveLength(2);
    remote.calls[1]!.resolve("data:new");
    await flush();
    expect(store.urlFor("alice@example.com")).toBe("data:new");
  });

  test("avatarChanged mid-fetch drops the outdated answer and refetches", async () => {
    const { store, remote } = setup();
    store.retain("alice@example.com");
    store.handleAvatarChanged("alice@example.com", "sha1-new");
    expect(remote.calls).toHaveLength(1);

    remote.calls[0]!.resolve("data:old");
    await flush();
    expect(store.urlFor("alice@example.com")).toBeNull();
    expect(remote.calls).toHaveLength(2);
    remote.calls[1]!.resolve("data:new");
    await flush();
    expect(store.urlFor("alice@example.com")).toBe("data:new");
  });

  test("avatarChanged without an id clears to initials immediately, without a fetch", async () => {
    const { store, remote } = setup();
    store.retain("alice@example.com");
    remote.calls[0]!.resolve("data:a");
    await flush();

    store.handleAvatarChanged("alice@example.com");
    expect(store.urlFor("alice@example.com")).toBeNull();
    expect(remote.calls).toHaveLength(1);
  });

  test("a disable that races an in-flight fetch wins", async () => {
    const { store, remote } = setup();
    store.retain("alice@example.com");
    store.handleAvatarChanged("alice@example.com");
    remote.calls[0]!.resolve("data:stale");
    await flush();
    expect(store.urlFor("alice@example.com")).toBeNull();
  });

  test("isKnownAbsent separates a definitive none from not-yet-resolved and failures", async () => {
    const { store, remote } = setup();
    expect(store.isKnownAbsent("alice@example.com")).toBe(false);
    store.retain("alice@example.com");
    store.retain("bob@example.com");
    remote.calls[0]!.resolve(null);
    remote.calls[1]!.reject(new Error("timeout"));
    await flush();
    expect(store.isKnownAbsent("alice@example.com")).toBe(true);
    expect(store.isKnownAbsent("bob@example.com")).toBe(false);

    store.handleAvatarChanged("carol@example.com");
    expect(store.isKnownAbsent("carol@example.com")).toBe(true);
  });

  test("avatarChanged for a JID nobody has shown is not fetched eagerly", () => {
    const { store, remote } = setup();
    store.handleAvatarChanged("stranger@example.com", "sha1");
    expect(remote.calls).toHaveLength(0);
  });

  test("invalidate refetches a known JID (own profile republish)", async () => {
    const { store, remote } = setup();
    store.retain("me@example.com");
    remote.calls[0]!.resolve(null);
    await flush();
    store.invalidate("me@example.com");
    expect(remote.calls).toHaveLength(2);
  });

  test("reset forgets every result and drops in-flight answers", async () => {
    const { store, remote } = setup();
    store.retain("alice@example.com");
    store.reset();
    remote.calls[0]!.resolve("data:a");
    await flush();
    expect(store.urlFor("alice@example.com")).toBeNull();
  });
});
