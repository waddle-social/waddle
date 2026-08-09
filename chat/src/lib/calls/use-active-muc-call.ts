import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import { $callState } from "./call-store";
import {
  $mucCallMedia,
  $mucCallParticipantOwners,
  $mucCallParticipants,
  mucCallMediaForRoom,
  normalizeMucCallRoomJid,
} from "./muc-call-presence";
import {
  $mucCallLeavingRooms,
  $mucCallLiveParticipants,
  mucCallRoomLeavingNick,
} from "./muc-call-live-participants";
import {
  $mucCallTerminatePendingSessions,
  pendingMucCallTerminateSession,
} from "./muc-call-session-cache";
import type { CallMedia } from "./types";
import { connectionStore } from "@/lib/connection-store";
import { fullJidIdentityKey, jidLocalpart } from "@/lib/xmpp/jid";

/**
 * Derived view of "is the local user currently in an active MUC group
 * call, and which room owns it?"
 *
 * Drives:
 * - MUC ownership gates in the split/expanded call surfaces.
 * - `CallActivePill` local-resource visibility checks.
 * - `ChatHeader` indicator wiring.
 *
 * The selector intentionally treats DM calls (`kind: "dm"`) as
 * out-of-scope: DM call surfaces match directly on peer JID, while
 * MUC calls need room ownership plus Muji participant state.
 */
export function useActiveMucCall(): {
  /** Bare room JID of the active MUC call, or null when no MUC call
   *  is in flight (idle, ended, DM, ringing, etc.). */
  activeRoomJid: ComputedRef<string | null>;
  /** Whether the local user's MUC nick currently appears in the call's
   *  Muji-active participant list. */
  selfInCall: ComputedRef<boolean>;
  /** Convenience: count of nicks advertising active Muji presence in
   *  the call's room. Mirrors what `MucCallButton` displays. */
  participantCount: ComputedRef<number>;
  media: ComputedRef<CallMedia>;
} {
  const state = useStore($callState);
  const participants = useStore($mucCallParticipants);
  const callMedia = useStore($mucCallMedia);

  const activeRoomJid = computed<string | null>(() => {
    const s = state.value;
    if (s.phase !== "active" || s.kind !== "muc") return null;
    const normalized = normalizeMucCallRoomJid(s.peer);
    return normalized || null;
  });

  const selfInCall = computed<boolean>(() => {
    const roomJid = activeRoomJid.value;
    if (!roomJid) return false;
    const nick = connectionStore.session?.username;
    if (!nick) return false;
    const nicks = participants.value[roomJid] ?? [];
    return nicks.includes(nick);
  });

  const participantCount = computed<number>(() => {
    const roomJid = activeRoomJid.value;
    if (!roomJid) return 0;
    return (participants.value[roomJid] ?? []).length;
  });

  const media = computed<CallMedia>(() => {
    const roomJid = activeRoomJid.value;
    return mediaForRoom(roomJid ?? "", state.value, callMedia.value);
  });

  return { activeRoomJid, selfInCall, participantCount, media };
}

/**
 * Per-room view of whether XEP-0272 Muji presence says a room has
 * an active call, independent of whether this local client is already
 * in that call. This is the selector for "click to join" affordances
 * after refresh: `$callState` starts idle on a fresh page load, while
 * `$mucCallParticipants` is repopulated by MUC roster/presence replay.
 */
export function useRoomHasActiveCall(
  roomJid: () => string,
): {
  hasActiveCall: ComputedRef<boolean>;
  selfInCall: ComputedRef<boolean>;
  localResourceInCall: ComputedRef<boolean>;
  participantCount: ComputedRef<number>;
  media: ComputedRef<CallMedia>;
} {
  const state = useStore($callState);
  const participants = useStore($mucCallParticipants);
  const participantOwners = useStore($mucCallParticipantOwners);
  const callMedia = useStore($mucCallMedia);
  const pendingTerminates = useStore($mucCallTerminatePendingSessions);
  const liveParticipants = useStore($mucCallLiveParticipants);
  const leavingRooms = useStore($mucCallLeavingRooms);

  const normalizedRoomJid = computed<string | null>(() => {
    const normalized = normalizeMucCallRoomJid(roomJid());
    return normalized || null;
  });

  /**
   * Prefer LiveKit's authoritative participant set when we are
   * connected to this room's call. The LK projection arrives within
   * seconds of the SFU's truth changing — typically before the
   * MUC server's XEP-0198 ping detects a dead resource and the
   * webhook bridge fires — eliminating the ghost-while-in-call
   * symptom. When we are NOT in this room's call, the LK projection
   * is empty for this room and we fall back to the Muji-derived
   * view (kept honest by the server-side LK→MUC webhook bridge).
   */
  const participantList = computed<ReadonlyArray<string>>(() =>
    resolveRoomParticipantList(
      normalizedRoomJid.value,
      participants.value,
      participantOwners.value,
      liveParticipants.value,
      leavingRooms.value,
    ),
  );

  const pendingTerminateSession = computed(() =>
    pendingMucCallTerminateSession(
      pendingTerminates.value,
      normalizedRoomJid.value ?? "",
      currentClientFullJid(),
    )
  );
  const pendingTerminate = computed<boolean>(() => !!pendingTerminateSession.value);

  const hasActiveCall = computed<boolean>(() => participantList.value.length > 0 || pendingTerminate.value);

  const selfInCall = computed<boolean>(() => {
    const nick = connectionStore.session?.username;
    return !!nick && (participantList.value.includes(nick) || pendingTerminate.value);
  });

  const localResourceInCall = computed<boolean>(() => {
    const room = normalizedRoomJid.value;
    const s = state.value;
    if (!room) return false;
    if (s.phase === "active" && s.kind === "muc" && normalizeMucCallRoomJid(s.peer) === room) {
      return true;
    }
    const fullJid = currentClientFullJid();
    const owners = participantOwners.value[room] ?? [];
    return hasOwnerFullJid(owners, fullJid) ||
      hasUnownedSelfNick(owners, connectionStore.session?.username ?? null, participantList.value) ||
      !!pendingMucCallTerminateSession(pendingTerminates.value, room, fullJid);
  });

  const participantCount = computed<number>(() => Math.max(participantList.value.length, pendingTerminate.value ? 1 : 0));

  const media = computed<CallMedia>(() =>
    mediaForRoomWithPendingFallback(
      normalizedRoomJid.value ?? "",
      state.value,
      pendingTerminateSession.value?.media,
      callMedia.value,
    )
  );

  return { hasActiveCall, selfInCall, localResourceInCall, participantCount, media };
}

export function readRoomHasActiveCall(roomJid: string): {
  hasActiveCall: boolean;
  selfInCall: boolean;
  localResourceInCall: boolean;
  participantCount: number;
  media: CallMedia;
} {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) {
    return {
      hasActiveCall: false,
      selfInCall: false,
      localResourceInCall: false,
      participantCount: 0,
      media: mucCallMediaForRoom(""),
    };
  }
  const participantOwners = $mucCallParticipantOwners.get()[normalized] ?? [];
  const liveParticipants = $mucCallLiveParticipants.get();
  const participantList = resolveRoomParticipantList(
    normalized,
    $mucCallParticipants.get(),
    $mucCallParticipantOwners.get(),
    liveParticipants,
    $mucCallLeavingRooms.get(),
  );
  const pendingTerminates = $mucCallTerminatePendingSessions.get();
  const nick = connectionStore.session?.username ?? null;
  const fullJid = currentClientFullJid();
  const s = $callState.get();
  const pendingTerminateSession = pendingMucCallTerminateSession(pendingTerminates, normalized, fullJid);
  const pendingTerminate = !!pendingTerminateSession;
  return {
    hasActiveCall: participantList.length > 0 || pendingTerminate,
    selfInCall: !!nick && (participantList.includes(nick) || pendingTerminate),
    localResourceInCall:
      (
        s.phase === "active" &&
        s.kind === "muc" &&
        normalizeMucCallRoomJid(s.peer) === normalized
      ) ||
      hasOwnerFullJid(participantOwners, fullJid) ||
      hasUnownedSelfNick(participantOwners, nick, participantList) ||
      pendingTerminate,
    participantCount: Math.max(participantList.length, pendingTerminate ? 1 : 0),
    media: mediaForRoomWithPendingFallback(
      normalized,
      s,
      pendingTerminateSession?.media,
      $mucCallMedia.get(),
    ),
  };
}

function currentClientFullJid(): string {
  return fullJidIdentityKey(
    connectionStore.selfFullJid ??
    (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid,
  );
}

function hasOwnerFullJid(
  owners: ReadonlyArray<{ realJid?: string }>,
  fullJid: string,
): boolean {
  if (!fullJid) return false;
  return owners.some((owner) => fullJidIdentityKey(owner.realJid) === fullJid);
}

function hasUnownedSelfNick(
  owners: ReadonlyArray<{ nick: string; realJid?: string }>,
  nick: string | null | undefined,
  participants: ReadonlyArray<string>,
): boolean {
  if (!nick || !participants.includes(nick)) return false;
  return owners.some((owner) => owner.nick === nick && !fullJidIdentityKey(owner.realJid));
}

/**
 * Resolve the nick list for a room, preferring LiveKit's live
 * participant projection when populated (same semantics as
 * `useRoomHasActiveCall`'s composable path).
 *
 * When the room carries a transient "leaving" marker (the local user
 * just disconnected and the LK projection was cleared), our own stale
 * Muji nick is excluded from the fallback list so the count decreases
 * monotonically to 0. The marker is consumed once the Muji view drops
 * our nick on its own — at which point keeping it would needlessly
 * mask a genuine re-join.
 */
function resolveRoomParticipantList(
  roomJid: string | null,
  participantsByRoom: Readonly<Record<string, ReadonlyArray<string>>>,
  ownersByRoom: Readonly<Record<string, ReadonlyArray<{ nick: string; realJid?: string }>>>,
  liveParticipantsByRoom: Readonly<Record<string, ReadonlyArray<string>>>,
  leavingByRoom: Readonly<Record<string, string>> = {},
): string[] {
  const room = roomJid ? normalizeMucCallRoomJid(roomJid) : "";
  if (!room) return [];
  const liveIdentities = liveParticipantsByRoom[room] ?? [];
  if (liveIdentities.length > 0) {
    return identitiesToNicks(liveIdentities, ownersByRoom[room] ?? []);
  }
  const mujiNicks = participantsByRoom[room] ?? [];
  const leavingNick = mucCallRoomLeavingNick(leavingByRoom, room);
  // Pure read: when the room is locally leaving and Muji still lists
  // our own nick, suppress it. Once Muji drops the nick the marker is
  // a no-op here; it is cleared by `applyMucCallPresence` (the
  // server-authoritative drop) or by a fresh (re)join.
  if (!leavingNick) return [...mujiNicks];
  return mujiNicks.filter((nick) => nick !== leavingNick);
}

/**
 * Map LiveKit participant identities (full JIDs) to MUC nicks via
 * the cached owner list, deduplicating identical nicks (the same
 * person on two LK sessions). Identities without a known owner
 * mapping degrade to the bare localpart so the UI still has a
 * sensible label — once the Muji presence catches up the owner
 * mapping resolves and the next render uses the canonical nick.
 */
function identitiesToNicks(
  identities: ReadonlyArray<string>,
  owners: ReadonlyArray<{ nick: string; realJid?: string }>,
): string[] {
  const byRealJid = new Map<string, string>();
  for (const owner of owners) {
    const key = fullJidIdentityKey(owner.realJid);
    if (key) byRealJid.set(key, owner.nick);
  }
  const seen = new Set<string>();
  const out: string[] = [];
  for (const identity of identities) {
    const key = fullJidIdentityKey(identity);
    if (!key) continue;
    // Fall back to the JID's localpart (everything before `@`) when
    // the Muji-derived owner map hasn't resolved this resource yet.
    // Earlier this incorrectly used `identity.split("/").pop()` which
    // returned the *resource* (e.g. `web` for `alice@host/web`); the
    // localpart is the correct user-facing label until presence catches
    // up. After: `alice` for `alice@host/web`.
    const nick = byRealJid.get(key) ?? jidLocalpart(identity);
    if (seen.has(nick)) continue;
    seen.add(nick);
    out.push(nick);
  }
  return out;
}

/**
 * Imperative variant for non-component code paths (e.g. unit tests or
 * effect runners that don't have a Vue setup context). Reads the
 * underlying stores once; callers needing reactivity should use the
 * composable above.
 */
export function readActiveMucCall(): {
  activeRoomJid: string | null;
  selfInCall: boolean;
  participantCount: number;
  media: CallMedia;
} {
  const s = $callState.get();
  if (s.phase !== "active" || s.kind !== "muc") {
    return { activeRoomJid: null, selfInCall: false, participantCount: 0, media: mucCallMediaForRoom("") };
  }
  const roomJid = normalizeMucCallRoomJid(s.peer) || null;
  if (!roomJid) {
    return { activeRoomJid: null, selfInCall: false, participantCount: 0, media: mucCallMediaForRoom("") };
  }
  const nicks = $mucCallParticipants.get()[roomJid] ?? [];
  const nick = connectionStore.session?.username ?? null;
  return {
    activeRoomJid: roomJid,
    selfInCall: nick !== null && nicks.includes(nick),
    participantCount: nicks.length,
    media: mediaForRoom(roomJid, s),
  };
}

function mediaForRoom(
  roomJid: string,
  state: ReturnType<typeof $callState.get>,
  mediaByRoom?: Record<string, CallMedia>,
): CallMedia {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (
    normalized &&
    (state.phase === "active" || state.phase === "muc-pending") &&
    state.kind === "muc" &&
    normalizeMucCallRoomJid(state.peer) === normalized
  ) {
    return state.media;
  }
  return mucCallMediaForRoom(normalized, mediaByRoom);
}

function mediaForRoomWithPendingFallback(
  roomJid: string,
  state: ReturnType<typeof $callState.get>,
  pendingMedia?: CallMedia,
  mediaByRoom?: Record<string, CallMedia>,
): CallMedia {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) return mucCallMediaForRoom("");
  const liveMedia = mediaByRoom?.[normalized];
  if (liveMedia) return liveMedia;
  if (
    (state.phase === "active" || state.phase === "muc-pending") &&
    state.kind === "muc" &&
    normalizeMucCallRoomJid(state.peer) === normalized
  ) {
    return state.media;
  }
  return pendingMedia ?? mucCallMediaForRoom(normalized, mediaByRoom);
}
