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
import { removeRoomBookmark, saveRoomBookmark, turnOffRoomBookmarkAutojoin } from "@/lib/xmpp/room-bookmarks";
import {
  joinedSpaceKey,
  localSpaceUnfollowIntentStore,
  pepJoinedSpaceStore,
  unfollowOvertaken,
  type JoinedSpacePreference,
  type JoinedSpaceStore,
  type SpaceUnfollowIntent,
  type SpaceUnfollowIntentStore,
} from "@/lib/xmpp/joined-spaces";
import { classifyOwnDeparture, type RoomDeparture } from "@/lib/xmpp/remote-membership";
import { roomJoinRetryDelayMs } from "@/lib/xmpp/room-join-retry";
import type { RawIqClient } from "@/lib/xmpp/xml-stanza";
import type { OwnRoomDeparture } from "@/lib/xmpp/client-events";

/** Automatic retries of an outage before retrying is left to the user. */
const MAX_AUTOMATIC_RETRIES = 5;

/**
 * Spacing of the subscription renewals sent at sign-in: each followed
 * Space waits its turn plus a random share of one more step, so a user who
 * follows many Spaces does not send a burst to their hosts at once.
 */
const SIGN_IN_RENEWAL_SPACING_MS = 3_000;

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
  /** A join of the room waits for its host's answer, whoever started it. */
  joining: boolean;
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
  /**
   * The home deployment's addresses, with `mucService` only once it was
   * discovered. While it is not, no room counts as remote here.
   */
  home: ComputedRef<HomeAddresses | null>;
  /** Whether the home server federates; nothing remote is read or renewed until it does. */
  federation: Ref<boolean>;
  /** Reload the room list after a bookmark changed. */
  reloadRooms: () => Promise<unknown>;
  /** Open a room in the timeline; `false` when the room list does not hold it. */
  openRoom: (roomJid: string) => Promise<boolean>;
  /** Stop showing a room the user left, so nothing joins it again on its behalf. */
  closeRoom: (roomJid: string) => void;
  /** Overrides for tests. */
  createStore?: (xmpp: RawIqClient, ownBareJid: string) => JoinedSpaceStore;
  createIntentStore?: (ownBareJid: string) => SpaceUnfollowIntentStore;
  timer?: Timer;
  now?: () => number;
  random?: () => number;
}

const defaultTimer: Timer = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle),
};

/** Stop retrying an unsubscribe after this outcome: the host answered, or cannot. */
function unsubscribeSettled(failure: RemoteFailure): boolean {
  return failure.kind !== "outage";
}

/**
 * State of the communities on other servers that the user looks up, joins
 * and follows. Each action changes one record (see `remote-membership.ts`),
 * failures are kept apart as outage, denial or deletion, and only outages
 * are retried without the user asking.
 */
export function useRemoteCommunities(deps: RemoteCommunitiesDeps) {
  const timer = deps.timer ?? defaultTimer;
  const now = deps.now ?? (() => Date.now());
  const random = deps.random ?? Math.random;
  const createStore = deps.createStore ?? pepJoinedSpaceStore;
  const createIntentStore = deps.createIntentStore ?? ((own: string) => localSpaceUnfollowIntentStore(own));

  const lookup = ref<RemoteLookup>({ state: "idle" });
  const rooms = ref<RemoteRoomEntry[]>([]);
  const spaces = ref<RemoteSpaceEntry[]>([]);
  const isLoading = ref(false);
  const loadFailure = ref<RemoteFailure | null>(null);
  const joinedRoomKeys = ref<Set<string>>(new Set());
  const joiningRoomKeys = ref<Set<string>>(new Set());

  let generation = 0;
  let lookupRequest = 0;
  let renewedGeneration = -1;
  const retryTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const retryAttempts = new Map<string, number>();
  const renewalTimers = new Set<ReturnType<typeof setTimeout>>();
  /** Rooms the user left: an outage afterwards is no reason to join again. */
  const leftByUser = new Set<string>();
  /**
   * The latest user intent per room. A bookmark write runs only while its
   * intent is still the latest, and writes for one room run one at a time,
   * so the last of a join and a leave decides the bookmark's autojoin.
   */
  const roomIntents = new Map<string, number>();
  const bookmarkWrites = new Map<string, Promise<unknown>>();
  let intentSequence = 0;
  /** The `revision` of each Space's joined-space entry as last read or written here. */
  const listRevisions = new Map<string, string | undefined>();

  const sortedRooms = computed(() =>
    [...rooms.value]
      .map((room) => {
        const key = bareJidKey(room.roomJid);
        return { ...room, joined: joinedRoomKeys.value.has(key), joining: joiningRoomKeys.value.has(key) };
      })
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
        joining: false,
        departure: null,
        status: { state: "idle" },
        note: null,
        ...change,
      }];
      return;
    }
    rooms.value = rooms.value.map((room, position) => (position === index ? { ...room, ...change } : room));
  }

  function findRoom(roomJid: string): RemoteRoomEntry | undefined {
    const key = bareJidKey(roomJid);
    return rooms.value.find((room) => bareJidKey(room.roomJid) === key);
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

  function nextRoomIntent(key: string): number {
    intentSequence += 1;
    roomIntents.set(key, intentSequence);
    return intentSequence;
  }

  /**
   * Run a bookmark write for one room after the writes already queued for
   * it, and only if `intent` is still the user's latest for the room.
   * Resolves whether the write ran.
   */
  function writeBookmark(key: string, intent: number, write: () => Promise<void>): Promise<boolean> {
    const previous = bookmarkWrites.get(key) ?? Promise.resolve();
    const run = previous.then(async () => {
      if (roomIntents.get(key) !== intent) return false;
      await write();
      return true;
    });
    const settled = run.catch(() => undefined);
    bookmarkWrites.set(key, settled);
    void settled.then(() => {
      if (bookmarkWrites.get(key) === settled) bookmarkWrites.delete(key);
    });
    return run;
  }

  /**
   * Join a room on another server, then bookmark it so every device lists
   * and rejoins it. The join is the room host's decision; the bookmark is
   * written only once the host has seated the user, and only for a join the
   * user asked for: rejoining after a lost stream leaves the bookmark as
   * the user set it. A Leave the user chose while the join was under way
   * wins: the join's bookmark is not written.
   */
  async function joinRoom(
    roomJid: string,
    name?: string | null,
    options: { saveBookmark: boolean } = { saveBookmark: true },
  ): Promise<void> {
    const canonical = canonicalRoomJid(roomJid);
    if (!canonical) return;
    const key = bareJidKey(canonical);
    const retryKey = `room:${canonical}`;
    clearRetry(retryKey);
    const intent = options.saveBookmark ? nextRoomIntent(key) : null;
    if (options.saveBookmark) leftByUser.delete(key);
    const startedGeneration = generation;
    updateRoom(canonical, { status: { state: "working" }, departure: null, note: null, ...(name ? { name } : {}) });
    let context: ReturnType<typeof requireXmpp>;
    try {
      context = requireXmpp();
      await context.client.ensureJoined(canonical);
    } catch (error) {
      if (startedGeneration !== generation) return;
      const failure = classifyRemoteError(error);
      updateRoom(canonical, { status: failedStatus(retryKey, failure, () => void joinRoom(canonical, name, options)) });
      return;
    }
    if (startedGeneration !== generation) return;
    resetRetries(retryKey);
    // A Leave chosen while the join was under way decides what is shown;
    // the room's own presence says whether the user is still in it.
    if (intent !== null && roomIntents.get(key) !== intent) return;
    markJoined(canonical, true);
    updateRoom(canonical, { status: { state: "ready" } });
    if (intent === null) return;
    try {
      const wrote = await writeBookmark(key, intent, async () => {
        await saveRoomBookmark(context.xmpp, context.own, {
          roomJid: canonical,
          name: name ?? findRoom(canonical)?.name ?? undefined,
          autojoin: true,
        });
        updateRoom(canonical, { bookmarked: true, autojoin: true });
      });
      if (wrote) await deps.reloadRooms();
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
    const entry = canonical ? findRoom(canonical) : undefined;
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
    const key = bareJidKey(canonical);
    const intent = nextRoomIntent(key);
    leftByUser.add(key);
    resetRetries(`room:${canonical}`);
    updateRoom(canonical, { status: { state: "working" }, note: null });
    try {
      const { client, xmpp, own } = requireXmpp();
      await client.leaveRoom(canonical);
      markJoined(canonical, false);
      deps.closeRoom(canonical);
      try {
        await writeBookmark(key, intent, async () => {
          // The stored bookmark decides, read when the write runs: this
          // device's copy may be stale or not loaded, and a join's write
          // queued before this one may have turned autojoin on meanwhile.
          if (await turnOffRoomBookmarkAutojoin(xmpp, own, canonical)) {
            updateRoom(canonical, { autojoin: false });
          }
        });
      } catch {
        updateRoom(canonical, { note: "You left, but your other devices may still join this room at login." });
      }
      updateRoom(canonical, { status: { state: "idle" }, departure: "left" });
    } catch (error) {
      updateRoom(canonical, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
    }
  }

  /**
   * Forget a room: remove its bookmark. Offered only once the room is left
   * and no join of it is under way, so a forgotten room is never still
   * delivering traffic or joined a moment later.
   */
  async function forgetRoom(roomJid: string): Promise<void> {
    const canonical = canonicalRoomJid(roomJid);
    if (!canonical) return;
    const key = bareJidKey(canonical);
    if (joinedRoomKeys.value.has(key) || joiningRoomKeys.value.has(key)) return;
    if (deps.xmppClient.value?.isJoiningRoom?.(canonical)) return;
    if (findRoom(canonical)?.status.state === "working") return;
    const intent = nextRoomIntent(key);
    resetRetries(`room:${canonical}`);
    updateRoom(canonical, { status: { state: "working" }, note: null });
    try {
      const { xmpp, own } = requireXmpp();
      await writeBookmark(key, intent, () => removeRoomBookmark(xmpp, own, canonical));
      rooms.value = rooms.value.filter((room) => bareJidKey(room.roomJid) !== key);
      await deps.reloadRooms();
    } catch (error) {
      updateRoom(canonical, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
    }
  }

  function setKey(target: Ref<Set<string>>, key: string, present: boolean) {
    if (target.value.has(key) === present) return;
    const next = new Set(target.value);
    if (present) next.add(key);
    else next.delete(key);
    target.value = next;
  }

  function markJoined(roomJid: string, joined: boolean) {
    const key = bareJidKey(roomJid);
    setKey(joinedRoomKeys, key, joined);
    if (joined) setKey(joiningRoomKeys, key, false);
  }

  /**
   * The room ended the user's occupancy. A revocation (ban, removed
   * membership) keeps the bookmark and is never retried automatically; a
   * destroyed room is gone. An outage (the room's server shut down, or the
   * federation stream was lost) rejoins with backoff, but only a room that
   * was joined and that the user did not leave.
   */
  function handleDeparture(roomKey: string, statusCodes: readonly number[], details?: OwnRoomDeparture) {
    const key = bareJidKey(roomKey);
    const wasJoined = joinedRoomKeys.value.has(key);
    markJoined(roomKey, false);
    setKey(joiningRoomKeys, key, false);
    if (!isRemoteRoom(roomKey)) return;
    const departure = classifyOwnDeparture(statusCodes, { destroyed: details?.destroyed ?? false });
    const entry = findRoom(roomKey);
    const roomJid = entry?.roomJid ?? roomKey;
    const retryKey = `room:${roomJid}`;
    if (departure === "left") {
      if (entry) updateRoom(entry.roomJid, { departure });
      return;
    }
    if (departure === "destroyed") {
      resetRetries(retryKey);
      updateRoom(roomJid, {
        departure,
        status: { state: "failed", failure: { kind: "deletion", retry: "none", condition: "gone" }, retryAt: null },
      });
      return;
    }
    if (departure === "revoked") {
      resetRetries(retryKey);
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
    if (!wasJoined || leftByUser.has(key)) {
      updateRoom(roomJid, { departure });
      return;
    }
    updateRoom(roomJid, {
      departure,
      status: failedStatus(
        retryKey,
        { kind: "outage", retry: "automatic" },
        () => {
          // The user may have left meanwhile, or the home server may have
          // stopped federating; neither leaves a reason to join.
          if (!leftByUser.has(key) && deps.federation.value) void joinRoom(roomJid, null, { saveBookmark: false });
        },
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
    setKey(joiningRoomKeys, bareJidKey(roomKey), false);
    const failure = classifyRemoteError(error);
    // The client reports a missing room only for a room on another server,
    // so this does not wait for the home addresses to be known here.
    if (failure.roomMissing) {
      handleMissingRoom(roomKey, failure);
      return;
    }
    if (!isRemoteRoom(roomKey)) return;
    const entry = findRoom(roomKey);
    updateRoom(entry?.roomJid ?? roomKey, {
      status: { state: "failed", failure, retryAt: null },
    });
  }

  /**
   * A join found the room did not exist. Its bookmark's `autojoin` is
   * turned off, through the same queue as the user's own join and leave,
   * so no device joins it at sign-in again (each would create the room
   * and leave it). The bookmark stays; the user may forget it.
   */
  function handleMissingRoom(roomKey: string, failure: RemoteFailure) {
    const canonical = canonicalRoomJid(roomKey);
    if (!canonical) return;
    const key = bareJidKey(canonical);
    resetRetries(`room:${canonical}`);
    markJoined(canonical, false);
    updateRoom(findRoom(canonical)?.roomJid ?? canonical, {
      status: { state: "failed", failure, retryAt: null },
    });
    let context: ReturnType<typeof requireXmpp>;
    try {
      context = requireXmpp();
    } catch {
      return;
    }
    const intent = nextRoomIntent(key);
    const startedGeneration = generation;
    // The room list is read again only when a bookmark was rewritten: a
    // reload with nothing changed would only show the same list.
    let rewritten = false;
    void writeBookmark(key, intent, async () => {
      rewritten = await turnOffRoomBookmarkAutojoin(context.xmpp, context.own, canonical);
      if (rewritten && startedGeneration === generation) updateRoom(canonical, { autojoin: false });
    }).then(
      () => {
        if (rewritten && startedGeneration === generation) void deps.reloadRooms();
      },
      () => {
        if (startedGeneration !== generation) return;
        updateRoom(canonical, {
          note: "Your bookmark of this room still joins it at sign-in; your other devices will try it again.",
        });
      },
    );
  }

  function handleJoinStarted(roomKey: string) {
    if (isRemoteRoom(roomKey)) setKey(joiningRoomKeys, bareJidKey(roomKey), true);
  }

  // ── Spaces ───────────────────────────────────────────────────────────

  function store(): JoinedSpaceStore {
    const { xmpp, own } = requireXmpp();
    return createStore(xmpp, own);
  }

  function intentStore(): SpaceUnfollowIntentStore | null {
    const own = ownBareJid();
    return own ? createIntentStore(own) : null;
  }

  function loadIntents(): SpaceUnfollowIntent[] {
    return intentStore()?.load() ?? [];
  }

  function findIntent(key: string): SpaceUnfollowIntent | undefined {
    return loadIntents().find((intent) => joinedSpaceKey(intent) === key);
  }

  /** Record (or, with `null`, drop) the unconfirmed unfollow of one Space. */
  function setIntent(key: string, intent: SpaceUnfollowIntent | null) {
    const intents = intentStore();
    if (!intents) return;
    const others = intents.load().filter((candidate) => joinedSpaceKey(candidate) !== key);
    const settled = intent && intent.remoteDone && intent.preferenceDone;
    intents.save(intent && !settled ? [...others, intent] : others);
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
    // Following again replaces any unfollow this device still meant to finish.
    setIntent(key, null);
    updateSpace(preference, { following: true, subscription, status: { state: "ready" } });
    try {
      const entryName = name ?? findSpace(key)?.name ?? undefined;
      const revision = crypto.randomUUID();
      await createStore(context.xmpp, context.own).put({
        serviceJid,
        node,
        ...(entryName ? { name: entryName } : {}),
        follow: true,
        revision,
      });
      listRevisions.set(key, revision);
      updateSpace(preference, { listed: true });
    } catch {
      updateSpace(preference, {
        note: "You follow this Space, but it was not added to the list your other devices read.",
      });
    }
    void refreshSpace(preference);
  }

  /**
   * Write `follow` off to the Space's list entry, if the list still holds
   * it. Resolves the entry's revision afterwards.
   */
  async function writeUnfollowed(space: RemoteSpaceEntry): Promise<string | undefined> {
    const key = joinedSpaceKey(space);
    if (!space.listed) return listRevisions.get(key);
    const revision = crypto.randomUUID();
    await store().put({
      serviceJid: space.serviceJid,
      node: space.node,
      ...(space.name ? { name: space.name } : {}),
      follow: false,
      revision,
    });
    listRevisions.set(key, revision);
    return revision;
  }

  /**
   * Stop following a Space: end the subscription on its host, and record
   * that in the joined-space list. The Space stays listed and no room is
   * left. When the host cannot be reached, or refuses, the Space is
   * unfollowed here anyway and the unsubscribe is kept on this device, to be
   * sent again at sign-in (after an outage). Until the list says so too,
   * this device does not subscribe to the Space again.
   */
  async function unsubscribeSpace(key: string): Promise<void> {
    const space = findSpace(key);
    if (!space) return;
    resetRetries(`space:${key}`);
    updateSpace(space, { status: { state: "working" }, note: null });
    let context: ReturnType<typeof requireXmpp>;
    try {
      context = requireXmpp();
    } catch (error) {
      updateSpace(space, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
      return;
    }
    let remoteFailure: RemoteFailure | null = null;
    try {
      await unsubscribeFromSpace(context.xmpp, space.serviceJid, space.node, context.own);
    } catch (error) {
      remoteFailure = classifyRemoteError(error);
      if (remoteFailure.kind !== "outage" && remoteFailure.kind !== "denial") {
        updateSpace(space, { status: { state: "failed", failure: remoteFailure, retryAt: null } });
        return;
      }
    }
    const seenRevision = listRevisions.get(key);
    const intent: SpaceUnfollowIntent = {
      serviceJid: space.serviceJid,
      node: space.node,
      remoteDone: remoteFailure === null || unsubscribeSettled(remoteFailure),
      preferenceDone: false,
      ...(seenRevision ? { seenRevision } : {}),
    };
    setIntent(key, intent);
    // Unfollowed here whatever the host answered; what is left to finish
    // is said in the note, not as a failure of the Space.
    updateSpace(space, { following: false, subscription: null, status: { state: "ready" } });
    let note: string | null = remoteFailure
      ? remoteFailure.kind === "outage"
        ? "You no longer follow this Space here. Its server could not be told yet; Waddle tells it again at your next sign-in."
        : "You no longer follow this Space here, but its server refused to end the subscription."
      : null;
    try {
      const written = await writeUnfollowed(space);
      const { seenRevision: _previous, ...rest } = intent;
      setIntent(key, { ...rest, preferenceDone: true, ...(written ? { seenRevision: written } : {}) });
    } catch {
      note = note ?? "Your other devices may still renew this subscription.";
    }
    updateSpace(space, { note });
  }

  /**
   * Forget a Space: remove it from the joined-space list. Offered only once
   * the Space is no longer followed here. Its rooms are not touched, and an
   * unsubscribe its host has not confirmed is still sent at sign-in.
   */
  async function forgetSpace(key: string): Promise<void> {
    const space = findSpace(key);
    if (!space || space.following) return;
    resetRetries(`space:${key}`);
    updateSpace(space, { status: { state: "working" }, note: null });
    try {
      await store().remove(space);
      const intent = findIntent(key);
      if (intent) setIntent(key, { ...intent, preferenceDone: true });
      spaces.value = spaces.value.filter((candidate) => candidate.key !== key);
    } catch (error) {
      updateSpace(space, { status: { state: "failed", failure: classifyRemoteError(error), retryAt: null } });
    }
  }

  // ── Loading ──────────────────────────────────────────────────────────

  /**
   * Bring the Spaces' entries in line with the stored list and this
   * device's unconfirmed unfollows: a Space this device unfollowed is shown
   * unfollowed whatever the list still says, unless another device followed
   * it again since (its entry says to follow under a newer revision); such
   * an unfollow is dropped. Resolves the unfollows that still apply.
   */
  function applyPreferences(
    preferences: readonly JoinedSpacePreference[],
    intents: readonly SpaceUnfollowIntent[],
  ): SpaceUnfollowIntent[] {
    const entries = new Map(preferences.map((preference) => [joinedSpaceKey(preference), preference] as const));
    const current: SpaceUnfollowIntent[] = [];
    for (const intent of intents) {
      const key = joinedSpaceKey(intent);
      if (unfollowOvertaken(intent, entries.get(key))) setIntent(key, null);
      else current.push(intent);
    }
    const unfollowed = new Set(current.map(joinedSpaceKey));
    for (const preference of preferences) {
      const key = joinedSpaceKey(preference);
      listRevisions.set(key, preference.revision);
      updateSpace(preference, {
        name: findSpace(key)?.name ?? preference.name ?? null,
        listed: true,
        following: preference.follow && !unfollowed.has(key),
      });
    }
    return current;
  }

  /**
   * Read the user's remote rooms (from XEP-0402 bookmarks) and Spaces (from
   * the joined-space list), and ask each Space's host for its current
   * content. Subscriptions are renewed once per sign-in, not here.
   */
  async function load(): Promise<void> {
    const client = deps.xmppClient.value;
    if (!client || isLoading.value || !deps.federation.value) return;
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

      const preferences = await store().load();
      if (startedGeneration !== generation) return;
      applyPreferences(preferences, loadIntents());
      // The sign-in renewal could not read the list then; it can now.
      if (renewedGeneration !== generation) void renewAtSignIn();
      for (const preference of preferences) {
        const space = findSpace(joinedSpaceKey(preference));
        // A Space whose renewal failed keeps that status until it is retried.
        if (space && space.status.state !== "failed" && space.status.state !== "working") {
          void refreshSpace(preference);
        }
      }
    } catch (error) {
      if (startedGeneration === generation) loadFailure.value = classifyRemoteError(error);
    } finally {
      if (startedGeneration === generation) isLoading.value = false;
    }
  }

  /**
   * Subscribe again to a followed Space, as each sign-in does: a host may
   * let a subscription lapse that is not made again. An outage is retried
   * with backoff; a denial or a Space its host no longer knows is shown and
   * left to the user.
   */
  async function renewSubscription(preference: Pick<JoinedSpacePreference, "serviceJid" | "node">): Promise<void> {
    const key = joinedSpaceKey(preference);
    const startedGeneration = generation;
    if (!findSpace(key)?.following) return;
    try {
      const { xmpp, own } = requireXmpp();
      const subscription = await subscribeToSpace(xmpp, preference.serviceJid, preference.node, own);
      if (startedGeneration !== generation) return;
      resetRetries(`space:${key}`);
      updateSpace(preference, { subscription, status: { state: "ready" } }, { existingOnly: true });
    } catch (error) {
      if (startedGeneration !== generation || !findSpace(key)) return;
      updateSpace(preference, {
        subscription: null,
        status: failedStatus(`space:${key}`, classifyRemoteError(error), () => void renewSubscription(preference)),
      }, { existingOnly: true });
    }
  }

  /**
   * Send again an unsubscribe this device could not deliver, and write the
   * list entry an unfollow could not write. An outage leaves the record for
   * the next sign-in; any other answer from the host settles it.
   */
  async function finishUnfollow(intent: SpaceUnfollowIntent): Promise<void> {
    const key = joinedSpaceKey(intent);
    const startedGeneration = generation;
    let next = { ...intent };
    try {
      const { xmpp, own } = requireXmpp();
      // This runs a while after sign-in: read the list again, so a follow
      // another device wrote meanwhile is not undone here. A list that
      // cannot be read leaves the unfollow for the next sign-in.
      let entry: JoinedSpacePreference | undefined;
      try {
        entry = (await store().load()).find((preference) => joinedSpaceKey(preference) === key);
      } catch {
        return;
      }
      if (startedGeneration !== generation) return;
      if (unfollowOvertaken(intent, entry)) {
        setIntent(key, null);
        updateSpace(intent, { following: true }, { existingOnly: true });
        void renewSubscription(intent);
        return;
      }
      if (!next.remoteDone) {
        try {
          await unsubscribeFromSpace(xmpp, intent.serviceJid, intent.node, own);
          next = { ...next, remoteDone: true };
        } catch (error) {
          if (unsubscribeSettled(classifyRemoteError(error))) next = { ...next, remoteDone: true };
        }
      }
      if (startedGeneration !== generation) return;
      if (!next.preferenceDone) {
        const space = findSpace(key);
        if (!space?.listed) next = { ...next, preferenceDone: true };
        else {
          try {
            const written = await writeUnfollowed(space);
            const { seenRevision: _previous, ...rest } = next;
            next = { ...rest, preferenceDone: true, ...(written ? { seenRevision: written } : {}) };
          } catch {
            // Kept for the next sign-in.
          }
        }
      }
    } catch {
      return;
    }
    if (startedGeneration === generation) setIntent(key, next);
  }

  /**
   * At sign-in, once the home server federates: read the joined-space
   * list, then renew each followed Space's subscription and finish each
   * unconfirmed unfollow, one after another at spaced intervals. Results
   * show in the communities dialog only.
   */
  async function renewAtSignIn(): Promise<void> {
    if (renewedGeneration === generation || !deps.xmppClient.value || !deps.federation.value) return;
    renewedGeneration = generation;
    const startedGeneration = generation;
    let preferences: JoinedSpacePreference[];
    try {
      preferences = await store().load();
    } catch (error) {
      if (startedGeneration !== generation) return;
      loadFailure.value = classifyRemoteError(error);
      // The list could not be read: opening the communities dialog, or the
      // next sign-in, tries again.
      renewedGeneration = -1;
      return;
    }
    if (startedGeneration !== generation) return;
    const intents = applyPreferences(preferences, loadIntents());
    const unfollowed = new Set(intents.map(joinedSpaceKey));
    const tasks: Array<() => Promise<void>> = [
      ...intents.map((intent) => () => finishUnfollow(intent)),
      ...preferences
        .filter((preference) => preference.follow && !unfollowed.has(joinedSpaceKey(preference)))
        .map((preference) => () => renewSubscription(preference)),
    ];
    tasks.forEach((task, index) => {
      const delay = Math.round((index + random()) * SIGN_IN_RENEWAL_SPACING_MS);
      const handle = timer.setTimeout(() => {
        renewalTimers.delete(handle);
        if (startedGeneration === generation) void task();
      }, delay);
      renewalTimers.add(handle);
    });
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
    if (space.following) {
      void renewSubscription(space).then(() => {
        if (findSpace(key)?.status.state === "ready") return refreshSpace(space);
      });
    } else {
      void refreshSpace(space);
    }
  }

  function reset() {
    generation++;
    lookupRequest++;
    for (const handle of retryTimers.values()) timer.clearTimeout(handle);
    for (const handle of renewalTimers) timer.clearTimeout(handle);
    retryTimers.clear();
    renewalTimers.clear();
    retryAttempts.clear();
    leftByUser.clear();
    roomIntents.clear();
    bookmarkWrites.clear();
    listRevisions.clear();
    lookup.value = { state: "idle" };
    rooms.value = [];
    spaces.value = [];
    joinedRoomKeys.value = new Set();
    joiningRoomKeys.value = new Set();
    isLoading.value = false;
    loadFailure.value = null;
  }

  watch(deps.xmppClient, (client, _previous, onCleanup) => {
    reset();
    if (!client) return;
    const removeDeparture = client.onOwnRoomDeparture?.(handleDeparture);
    const removeJoinFailure = client.onRoomJoinFailed?.(handleJoinFailure);
    const removeJoinStarted = client.onRoomJoinStarted?.(handleJoinStarted);
    const removeNick = client.addOwnOccupantNickHandler?.((roomJid, nick) => {
      if (!nick) return;
      markJoined(roomJid, true);
      // Joined again, from wherever: a later outage rejoins it as before.
      leftByUser.delete(bareJidKey(roomJid));
    });
    onCleanup(() => {
      removeDeparture?.();
      removeJoinFailure?.();
      removeJoinStarted?.();
      removeNick?.();
    });
  }, { immediate: true, flush: "sync" });

  // Followed Spaces are renewed once per sign-in, as soon as the home
  // server is known to federate.
  watch([deps.xmppClient, deps.federation], ([client, federation]) => {
    if (client && federation) void renewAtSignIn();
  }, { immediate: true });

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
