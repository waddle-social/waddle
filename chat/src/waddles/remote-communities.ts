import { computed, ref, watch, type ComputedRef, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, bareJidKey } from "@/lib/xmpp/jid";
import {
  canonicalRoomJid,
  communityAddressDomain,
  isHomeDomain,
  isRemoteRoomJid,
  parseCommunityAddress,
  type CommunityAddress,
  type CommunityAddressError,
  type HomeAddresses,
} from "@/lib/xmpp/remote-address";
import { classifyRemoteError, type RemoteFailure } from "@/lib/xmpp/remote-outcome";
import {
  describeCommunityAddress,
  fetchSpaceContent,
  subscribeToSpace,
  unsubscribeFromSpace,
  type RemoteDescription,
  type RemoteSpaceContent,
  type SpaceSubscriptionState,
} from "@/lib/xmpp/remote-community";
import { removeRoomBookmark, saveRoomBookmark } from "@/lib/xmpp/room-bookmarks";
import {
  joinedSpaceKey,
  privateXmlJoinedSpaceStore,
  withJoinedSpace,
  withoutJoinedSpace,
  type JoinedSpacePreference,
  type JoinedSpaceStore,
} from "@/lib/xmpp/joined-spaces";
import { classifyOwnDeparture, type RoomDeparture } from "@/lib/xmpp/remote-membership";
import { roomJoinRetryDelayMs } from "@/lib/xmpp/room-join-retry";
import type { RawIqClient } from "@/lib/xmpp/xml-stanza";

/** Automatic retries of an outage before retrying is left to the user. */
const MAX_AUTOMATIC_RETRIES = 5;

export type RemoteStatus =
  | { state: "idle" }
  | { state: "working" }
  | { state: "ready" }
  | { state: "failed"; failure: RemoteFailure; retryAt: number | null };

export interface RemoteRoomEntry {
  roomJid: string;
  name: string | null;
  bookmarked: boolean;
  autojoin: boolean;
  joined: boolean;
  /** How the room last ended the user's occupancy, when it did. */
  departure: RoomDeparture | null;
  status: RemoteStatus;
  /** A follow-up that failed after the main action succeeded. */
  note: string | null;
}

export interface RemoteSpaceEntry {
  key: string;
  serviceJid: string;
  node: string;
  name: string | null;
  description: string | null;
  /** In the user's joined-space list. */
  listed: boolean;
  /** The user wants the Space's updates. */
  following: boolean;
  subscription: SpaceSubscriptionState | null;
  content: RemoteSpaceContent | null;
  status: RemoteStatus;
  note: string | null;
}

export type RemoteLookup =
  | { state: "idle" }
  | { state: "invalid"; error: CommunityAddressError }
  | { state: "home"; address: CommunityAddress }
  | { state: "working"; address: CommunityAddress }
  | {
    state: "found";
    address: CommunityAddress;
    description: RemoteDescription;
    content: RemoteSpaceContent | null;
    contentFailure: RemoteFailure | null;
  }
  | { state: "failed"; address: CommunityAddress; failure: RemoteFailure };

type Timer = {
  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout>;
  clearTimeout(handle: ReturnType<typeof setTimeout>): void;
};

export interface RemoteCommunitiesDeps {
  xmppClient: Ref<BrowserXmppClient | null>;
  session: Ref<WaddleSession | null>;
  home: ComputedRef<HomeAddresses | null>;
  /** Reload the room list after a bookmark changed. */
  reloadRooms: () => Promise<unknown>;
  /** Open a room in the timeline; `false` when the room list does not hold it. */
  openRoom: (roomJid: string) => Promise<boolean>;
  /** Stop showing a room the user left, so nothing joins it again on its behalf. */
  closeRoom: (roomJid: string) => void;
  /** Overrides for tests. */
  createStore?: (xmpp: RawIqClient, ownBareJid: string) => JoinedSpaceStore;
  timer?: Timer;
  now?: () => number;
}

const defaultTimer: Timer = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle),
};

/**
 * State of the communities on other servers that the user looks up, joins
 * and follows. Each action changes one record (see `remote-membership.ts`),
 * failures are kept apart as outage, denial or deletion, and only outages
 * are retried without the user asking.
 */
export function useRemoteCommunities(deps: RemoteCommunitiesDeps) {
  const timer = deps.timer ?? defaultTimer;
  const now = deps.now ?? (() => Date.now());
  const createStore = deps.createStore ?? privateXmlJoinedSpaceStore;

  const lookup = ref<RemoteLookup>({ state: "idle" });
  const rooms = ref<RemoteRoomEntry[]>([]);
  const spaces = ref<RemoteSpaceEntry[]>([]);
  const isLoading = ref(false);
  const loadFailure = ref<RemoteFailure | null>(null);
  const joinedRoomKeys = ref<Set<string>>(new Set());

  let generation = 0;
  let lookupRequest = 0;
  const retryTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const retryAttempts = new Map<string, number>();

  const sortedRooms = computed(() =>
    [...rooms.value]
      .map((room) => ({ ...room, joined: joinedRoomKeys.value.has(bareJidKey(room.roomJid)) }))
      .sort((a, b) => (a.name ?? a.roomJid).localeCompare(b.name ?? b.roomJid, undefined, { sensitivity: "base" })),
  );
  const sortedSpaces = computed(() =>
    [...spaces.value].sort((a, b) => (a.name ?? a.node).localeCompare(b.name ?? b.node, undefined, { sensitivity: "base" })),
  );

  function raw(): RawIqClient | null {
    return (deps.xmppClient.value?.agent as RawIqClient | null | undefined) ?? null;
  }

  function ownBareJid(): string | null {
    const jid = deps.session.value?.jid;
    return jid ? barePeerJid(jid) : null;
  }

  function requireXmpp(): { xmpp: RawIqClient; own: string; client: BrowserXmppClient } {
    const client = deps.xmppClient.value;
    const xmpp = raw();
    const own = ownBareJid();
    if (!client || !xmpp || !own) throw new Error("You're not connected right now.");
    return { xmpp, own, client };
  }

  function isRemoteRoom(roomJid: string): boolean {
    const home = deps.home.value;
    return !!home && isRemoteRoomJid(roomJid, home);
  }

  function clearRetry(key: string) {
    const handle = retryTimers.get(key);
    if (handle !== undefined) timer.clearTimeout(handle);
    retryTimers.delete(key);
  }

  function resetRetries(key: string) {
    clearRetry(key);
    retryAttempts.delete(key);
  }

  /**
   * The status a failure leaves. An outage that may be retried
   * automatically is scheduled, within a bound; after that the retry is the
   * user's.
   */
  function failedStatus(key: string, failure: RemoteFailure, retry: () => void): RemoteStatus {
    clearRetry(key);
    if (failure.retry !== "automatic") {
      retryAttempts.delete(key);
      return { state: "failed", failure, retryAt: null };
    }
    const attempt = retryAttempts.get(key) ?? 0;
    if (attempt >= MAX_AUTOMATIC_RETRIES) {
      return { state: "failed", failure: { ...failure, retry: "manual" }, retryAt: null };
    }
    retryAttempts.set(key, attempt + 1);
    const delay = roomJoinRetryDelayMs(attempt);
    const scheduledGeneration = generation;
    retryTimers.set(key, timer.setTimeout(() => {
      retryTimers.delete(key);
      if (scheduledGeneration === generation) retry();
    }, delay));
    return { state: "failed", failure, retryAt: now() + delay };
  }

  function updateRoom(roomJid: string, change: Partial<RemoteRoomEntry>) {
    const key = bareJidKey(roomJid);
    const index = rooms.value.findIndex((room) => bareJidKey(room.roomJid) === key);
    if (index < 0) {
      rooms.value = [...rooms.value, {
        roomJid: canonicalRoomJid(roomJid) ?? key,
        name: null,
        bookmarked: false,
        autojoin: false,
        joined: false,
        departure: null,
        status: { state: "idle" },
        note: null,
        ...change,
      }];
      return;
    }
    rooms.value = rooms.value.map((room, position) => (position === index ? { ...room, ...change } : room));
  }

  function findSpace(key: string): RemoteSpaceEntry | undefined {
    return spaces.value.find((space) => space.key === key);
  }

  /**
   * Apply a change to a Space's entry. An answer that arrives for a Space
   * the user forgot meanwhile (`existingOnly`) must not bring it back.
   */
  function updateSpace(
    preference: Pick<JoinedSpacePreference, "serviceJid" | "node">,
    change: Partial<RemoteSpaceEntry>,
    options: { existingOnly?: boolean } = {},
  ) {
    const key = joinedSpaceKey(preference);
    if (!key) return;
    if (!findSpace(key)) {
      if (options.existingOnly) return;
      spaces.value = [...spaces.value, {
        key,
        serviceJid: preference.serviceJid,
        node: preference.node,
        name: null,
        description: null,
        listed: false,
        following: false,
        subscription: null,
        content: null,
        status: { state: "idle" },
        note: null,
        ...change,
      }];
      return;
    }
    spaces.value = spaces.value.map((space) => (space.key === key ? { ...space, ...change } : space));
  }

  // ── Lookup ───────────────────────────────────────────────────────────

  async function lookUp(input: string): Promise<void> {
    const request = ++lookupRequest;
    const parsed = parseCommunityAddress(input);
    if (!parsed.ok) {
      lookup.value = { state: "invalid", error: parsed.error };
      return;
    }
    const address = parsed.address;
    const home = deps.home.value;
    if (home && isHomeDomain(communityAddressDomain(address), home)) {
      lookup.value = { state: "home", address };
      return;
    }
    lookup.value = { state: "working", address };
    try {
      const { xmpp } = requireXmpp();
      const description = await describeCommunityAddress(xmpp, address);
      if (request !== lookupRequest) return;
      let content: RemoteSpaceContent | null = null;
      let contentFailure: RemoteFailure | null = null;
      if (description.kind === "space") {
        try {
          content = await fetchSpaceContent(xmpp, description.serviceJid, description.node);
        } catch (error) {
          contentFailure = classifyRemoteError(error);
        }
        if (request !== lookupRequest) return;
      }
      lookup.value = { state: "found", address, description, content, contentFailure };
    } catch (error) {
      if (request !== lookupRequest) return;
      lookup.value = { state: "failed", address, failure: classifyRemoteError(error) };
    }
  }

  function clearLookup() {
    lookupRequest++;
    lookup.value = { state: "idle" };
  }

  // ── Rooms ────────────────────────────────────────────────────────────

  /**
   * Join a room on another server, then bookmark it so every device lists
   * and rejoins it. The join is the room host's decision; the bookmark is
   * written only once the host has seated the user, and only for a join the
   * user asked for: rejoining after a lost stream leaves the bookmark as
   * the user set it.
   */
  async function joinRoom(
    roomJid: string,
    name?: string | null,
    options: { saveBookmark: boolean } = { saveBookmark: true },
  ): Promise<void> {
    const canonical = canonicalRoomJid(roomJid);
    if (!canonical) return;
    const key = `room:${canonical}`;
    clearRetry(key);
    const startedGeneration = generation;
    updateRoom(canonical, { status: { state: "working" }, departure: null, note: null, ...(name ? { name } : {}) });
    let context: ReturnType<typeof requireXmpp>;
    try {
      context = requireXmpp();
      await context.client.ensureJoined(canonical);
    } catch (error) {
      if (startedGeneration !== generation) return;
      const failure = classifyRemoteError(error);
      updateRoom(canonical, { status: failedStatus(key, failure, () => void joinRoom(canonical, name, options)) });
      return;
    }
    if (startedGeneration !== generation) return;
    resetRetries(key);
    markJoined(canonical, true);
    updateRoom(canonical, { status: { state: "ready" } });
    if (!options.saveBookmark) return;
    const entry = rooms.value.find((room) => bareJidKey(room.roomJid) === bareJidKey(canonical));
    try {
      await saveRoomBookmark(context.xmpp, context.own, {
        roomJid: canonical,
        name: name ?? entry?.name ?? undefined,
        autojoin: true,
      });
      updateRoom(canonical, { bookmarked: true, autojoin: true });
      await deps.reloadRooms();
    } catch (error) {
      updateRoom(canonical, {
        note: error instanceof Error && error.name === "BookmarkNodeConfigError"
          ? error.message
          : "You joined, but the room's bookmark was not saved, so it is not in your room list or on your other devices.",
      });
    }
  }

  /** Join a room and show it. */
  async function joinAndOpenRoom(roomJid: string, name?: string | null): Promise<void> {
    await joinRoom(roomJid, name);
    const canonical = canonicalRoomJid(roomJid);
    const entry = canonical ? rooms.value.find((room) => bareJidKey(room.roomJid) === bareJidKey(canonical)) : null;
    if (!canonical || entry?.status.state !== "ready") return;
    if (!(await deps.openRoom(canonical))) {
      updateRoom(canonical, {
        note: entry.note ?? "This room can't be opened here until its bookmark is saved. Leave and join it again to retry.",
      });
    }
  }

  /**
   * Leave a room: end the occupancy and stop joining it at login. The
   * bookmark stays, so the room is still listed and can be joined again.
   */
  async function leaveRoom(roomJid: string): Promise<void> {
    const canonical = canonicalRoomJid(roomJid);
    if (!canonical) return;
    resetRetries(`room:${canonical}`);
    updateRoom(canonical, { status: { state: "working" }, note: null });
    try {
      const { client, xmpp, own } = requireXmpp();
      await client.leaveRoom(canonical);
      markJoined(canonical, false);
      deps.closeRoom(canonical);
      const entry = rooms.value.find((room) => bareJidKey(room.roomJid) === bareJidKey(canonical));
      if (entry?.bookmarked && entry.autojoin) {
        try {
          await saveRoomBookmark(xmpp, own, { roomJid: canonical, autojoin: false });
          updateRoom(canonical, { autojoin: false });
        } catch {
          updateRoom(canonical, { note: "You left, but your other devices may still join this room at login." });
        }
      }
      updateRoom(canonical, { status: { state: "idle" }, departure: "left" });
    } catch (error) {
      updateRoom(canonical, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
    }
  }

  /**
   * Forget a room: remove its bookmark. Offered only once the room is left,
   * so a forgotten room is never still delivering traffic.
   */
  async function forgetRoom(roomJid: string): Promise<void> {
    const canonical = canonicalRoomJid(roomJid);
    if (!canonical || joinedRoomKeys.value.has(bareJidKey(canonical))) return;
    resetRetries(`room:${canonical}`);
    updateRoom(canonical, { status: { state: "working" }, note: null });
    try {
      const { xmpp, own } = requireXmpp();
      await removeRoomBookmark(xmpp, own, canonical);
      rooms.value = rooms.value.filter((room) => bareJidKey(room.roomJid) !== bareJidKey(canonical));
      await deps.reloadRooms();
    } catch (error) {
      updateRoom(canonical, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
    }
  }

  function markJoined(roomJid: string, joined: boolean) {
    const key = bareJidKey(roomJid);
    if (joinedRoomKeys.value.has(key) === joined) return;
    const next = new Set(joinedRoomKeys.value);
    if (joined) next.add(key);
    else next.delete(key);
    joinedRoomKeys.value = next;
  }

  /**
   * The room ended the user's occupancy. A revocation (ban, removed
   * membership) keeps the bookmark and is never retried automatically; an
   * outage (the room's server shut down, or the federation stream was lost)
   * rejoins with backoff.
   */
  function handleDeparture(roomKey: string, statusCodes: readonly number[]) {
    markJoined(roomKey, false);
    if (!isRemoteRoom(roomKey)) return;
    const departure = classifyOwnDeparture(statusCodes);
    const entry = rooms.value.find((room) => bareJidKey(room.roomJid) === roomKey);
    if (departure === "left") {
      if (entry) updateRoom(entry.roomJid, { departure });
      return;
    }
    const roomJid = entry?.roomJid ?? roomKey;
    if (departure === "revoked") {
      resetRetries(`room:${roomJid}`);
      updateRoom(roomJid, {
        departure,
        status: {
          state: "failed",
          failure: {
            kind: "denial",
            retry: "manual",
            condition: statusCodes.includes(301) ? "forbidden" : "registration-required",
          },
          retryAt: null,
        },
      });
      return;
    }
    if (departure === "kicked") {
      updateRoom(roomJid, { departure, status: { state: "idle" } });
      return;
    }
    const key = `room:${roomJid}`;
    updateRoom(roomJid, {
      departure,
      status: failedStatus(
        key,
        { kind: "outage", retry: "automatic" },
        () => void joinRoom(roomJid, null, { saveBookmark: false }),
      ),
    });
  }

  /**
   * A join of a room on another server failed, whoever started it (a
   * sign-in autojoin, the room opened from the room list). Its status shows
   * the failure; retries of those joins belong to the client that started
   * them, so none is scheduled here.
   */
  function handleJoinFailure(roomKey: string, error: unknown) {
    if (!isRemoteRoom(roomKey)) return;
    const entry = rooms.value.find((room) => bareJidKey(room.roomJid) === roomKey);
    updateRoom(entry?.roomJid ?? roomKey, {
      status: { state: "failed", failure: classifyRemoteError(error), retryAt: null },
    });
  }

  // ── Spaces ───────────────────────────────────────────────────────────

  async function loadPreferences(): Promise<{ store: JoinedSpaceStore; preferences: JoinedSpacePreference[] }> {
    const { xmpp, own } = requireXmpp();
    const store = createStore(xmpp, own);
    return { store, preferences: await store.load() };
  }

  /** Read the Space's description and rooms from its host. */
  async function refreshSpace(preference: Pick<JoinedSpacePreference, "serviceJid" | "node">): Promise<void> {
    const key = joinedSpaceKey(preference);
    if (!key) return;
    const retryKey = `space:${key}`;
    clearRetry(retryKey);
    const startedGeneration = generation;
    if (!findSpace(key)) return;
    updateSpace(preference, { status: { state: "working" } });
    try {
      const { xmpp } = requireXmpp();
      const description = await describeCommunityAddress(xmpp, { kind: "space", ...preference });
      if (startedGeneration !== generation) return;
      if (description.kind !== "space") {
        resetRetries(retryKey);
        updateSpace(preference, {
          status: { state: "failed", failure: { kind: "deletion", retry: "none" }, retryAt: null },
        }, { existingOnly: true });
        return;
      }
      const content = await fetchSpaceContent(xmpp, preference.serviceJid, preference.node);
      if (startedGeneration !== generation) return;
      resetRetries(retryKey);
      updateSpace(preference, {
        name: description.title ?? findSpace(key)?.name ?? null,
        description: description.description,
        content,
        status: { state: "ready" },
      }, { existingOnly: true });
    } catch (error) {
      if (startedGeneration !== generation || !findSpace(key)) return;
      updateSpace(preference, {
        status: failedStatus(retryKey, classifyRemoteError(error), () => void refreshSpace(preference)),
      }, { existingOnly: true });
    }
  }

  /**
   * Follow a Space: subscribe to it on its host, then record it in the
   * joined-space list. The list entry is written only after the host
   * accepted, and it grants nothing.
   */
  async function followSpace(serviceJid: string, node: string, name?: string | null): Promise<void> {
    const preference = { serviceJid, node };
    const key = joinedSpaceKey(preference);
    if (!key) return;
    updateSpace(preference, { status: { state: "working" }, note: null, ...(name ? { name } : {}) });
    let subscription: SpaceSubscriptionState;
    let context: ReturnType<typeof requireXmpp>;
    try {
      context = requireXmpp();
      subscription = await subscribeToSpace(context.xmpp, serviceJid, node, context.own);
    } catch (error) {
      updateSpace(preference, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
      return;
    }
    updateSpace(preference, { following: true, subscription, status: { state: "ready" } });
    try {
      const store = createStore(context.xmpp, context.own);
      const current = await store.load();
      const entryName = name ?? findSpace(key)?.name ?? undefined;
      await store.save(withJoinedSpace(current, {
        serviceJid,
        node,
        ...(entryName ? { name: entryName } : {}),
        follow: true,
      }));
      updateSpace(preference, { listed: true });
    } catch {
      updateSpace(preference, {
        note: "You follow this Space, but it was not added to the list your other devices read.",
      });
    }
    void refreshSpace(preference);
  }

  /**
   * Stop following a Space: end the subscription on its host. The Space
   * stays in the joined-space list, and no room is left.
   */
  async function unsubscribeSpace(key: string): Promise<void> {
    const space = findSpace(key);
    if (!space) return;
    updateSpace(space, { status: { state: "working" }, note: null });
    let context: ReturnType<typeof requireXmpp>;
    try {
      context = requireXmpp();
      await unsubscribeFromSpace(context.xmpp, space.serviceJid, space.node, context.own);
    } catch (error) {
      updateSpace(space, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
      return;
    }
    updateSpace(space, { following: false, subscription: null, status: { state: "ready" } });
    try {
      const store = createStore(context.xmpp, context.own);
      const current = await store.load();
      const existing = current.find((preference) => joinedSpaceKey(preference) === key);
      if (existing) await store.save(withJoinedSpace(current, { ...existing, follow: false }));
    } catch {
      updateSpace(space, { note: "Your other devices may still renew this subscription." });
    }
  }

  /**
   * Forget a Space: remove it from the joined-space list. Offered only once
   * the Space is no longer followed. Its rooms are not touched.
   */
  async function forgetSpace(key: string): Promise<void> {
    const space = findSpace(key);
    if (!space || space.following) return;
    resetRetries(`space:${key}`);
    updateSpace(space, { status: { state: "working" }, note: null });
    try {
      const { store, preferences } = await loadPreferences();
      await store.save(withoutJoinedSpace(preferences, space));
      spaces.value = spaces.value.filter((candidate) => candidate.key !== key);
    } catch (error) {
      updateSpace(space, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
    }
  }

  // ── Loading ──────────────────────────────────────────────────────────

  /**
   * Read the user's remote rooms (from XEP-0402 bookmarks) and Spaces (from
   * the joined-space list), renew the subscriptions of followed Spaces, and
   * ask each host for its current state.
   */
  async function load(): Promise<void> {
    const client = deps.xmppClient.value;
    if (!client || isLoading.value) return;
    const startedGeneration = generation;
    isLoading.value = true;
    loadFailure.value = null;
    try {
      const bookmarks = await client.fetchUserBookmarks();
      if (startedGeneration !== generation) return;
      const remoteBookmarks = bookmarks.filter((bookmark) => isRemoteRoom(bookmark.jid));
      const bookmarkedKeys = new Set(remoteBookmarks.map((bookmark) => bareJidKey(bookmark.jid)));
      rooms.value = rooms.value.filter((room) => bookmarkedKeys.has(bareJidKey(room.roomJid)) || room.status.state !== "idle");
      for (const bookmark of remoteBookmarks) {
        updateRoom(bookmark.jid, {
          name: bookmark.name ?? null,
          bookmarked: true,
          autojoin: bookmark.autojoin,
        });
      }

      const { preferences } = await loadPreferences();
      if (startedGeneration !== generation) return;
      for (const preference of preferences) {
        updateSpace(preference, {
          name: findSpace(joinedSpaceKey(preference))?.name ?? preference.name ?? null,
          listed: true,
          following: preference.follow,
        });
      }
      for (const preference of preferences) {
        if (preference.follow) void renewSubscription(preference);
        else void refreshSpace(preference);
      }
    } catch (error) {
      if (startedGeneration === generation) loadFailure.value = classifyRemoteError(error);
    } finally {
      if (startedGeneration === generation) isLoading.value = false;
    }
  }

  /**
   * A host may let a subscription lapse that is not made again, so a
   * followed Space is subscribed again at each load before it is read.
   */
  async function renewSubscription(preference: JoinedSpacePreference): Promise<void> {
    const key = joinedSpaceKey(preference);
    const startedGeneration = generation;
    try {
      const { xmpp, own } = requireXmpp();
      const subscription = await subscribeToSpace(xmpp, preference.serviceJid, preference.node, own);
      if (startedGeneration !== generation) return;
      updateSpace(preference, { subscription }, { existingOnly: true });
    } catch (error) {
      if (startedGeneration !== generation || !findSpace(key)) return;
      updateSpace(preference, {
        subscription: null,
        status: failedStatus(`space:${key}`, classifyRemoteError(error), () => void renewSubscription(preference)),
      }, { existingOnly: true });
      return;
    }
    await refreshSpace(preference);
  }

  /** Retry a failed entry now. */
  function retryRoom(roomJid: string) {
    const canonical = canonicalRoomJid(roomJid);
    if (!canonical) return;
    retryAttempts.delete(`room:${canonical}`);
    void joinRoom(canonical);
  }

  function retrySpace(key: string) {
    const space = findSpace(key);
    if (!space) return;
    retryAttempts.delete(`space:${key}`);
    if (space.following) void renewSubscription({ serviceJid: space.serviceJid, node: space.node, follow: true });
    else void refreshSpace(space);
  }

  function reset() {
    generation++;
    lookupRequest++;
    for (const handle of retryTimers.values()) timer.clearTimeout(handle);
    retryTimers.clear();
    retryAttempts.clear();
    lookup.value = { state: "idle" };
    rooms.value = [];
    spaces.value = [];
    joinedRoomKeys.value = new Set();
    isLoading.value = false;
    loadFailure.value = null;
  }

  watch(deps.xmppClient, (client, _previous, onCleanup) => {
    reset();
    if (!client) return;
    const removeDeparture = client.onOwnRoomDeparture?.(handleDeparture);
    const removeJoinFailure = client.onRoomJoinFailed?.(handleJoinFailure);
    const removeNick = client.addOwnOccupantNickHandler?.((roomJid, nick) => {
      if (nick) markJoined(roomJid, true);
    });
    onCleanup(() => {
      removeDeparture?.();
      removeJoinFailure?.();
      removeNick?.();
    });
  }, { immediate: true, flush: "sync" });

  return {
    lookup,
    rooms: sortedRooms,
    spaces: sortedSpaces,
    isLoading,
    loadFailure,
    lookUp,
    clearLookup,
    load,
    joinRoom,
    joinAndOpenRoom,
    leaveRoom,
    forgetRoom,
    retryRoom,
    followSpace,
    unsubscribeSpace,
    forgetSpace,
    refreshSpace,
    retrySpace,
  };
}

export type RemoteCommunities = ReturnType<typeof useRemoteCommunities>;
