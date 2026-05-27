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
  $mucCallTerminatePendingSessions,
  hasPendingMucCallTerminateSession,
} from "./muc-call-session-cache";
import type { CallMedia } from "./types";
import { connectionStore } from "@/lib/connection-store";
import { fullJidIdentityKey } from "@/lib/xmpp/jid";

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

  const normalizedRoomJid = computed<string | null>(() => {
    const normalized = normalizeMucCallRoomJid(roomJid());
    return normalized || null;
  });

  const participantList = computed<ReadonlyArray<string>>(() => {
    const room = normalizedRoomJid.value;
    if (!room) return [];
    return participants.value[room] ?? [];
  });

  const pendingTerminate = computed<boolean>(() =>
    hasPendingMucCallTerminateSession(
      pendingTerminates.value,
      normalizedRoomJid.value ?? "",
      currentClientFullJid(),
    )
  );

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
    return hasOwnerFullJid(participantOwners.value[room] ?? [], fullJid) ||
      hasPendingMucCallTerminateSession(pendingTerminates.value, room, fullJid);
  });

  const participantCount = computed<number>(() => Math.max(participantList.value.length, pendingTerminate.value ? 1 : 0));

  const media = computed<CallMedia>(() =>
    mediaForRoom(normalizedRoomJid.value ?? "", state.value, callMedia.value)
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
  const participants = $mucCallParticipants.get()[normalized] ?? [];
  const participantOwners = $mucCallParticipantOwners.get()[normalized] ?? [];
  const pendingTerminates = $mucCallTerminatePendingSessions.get();
  const nick = connectionStore.session?.username ?? null;
  const fullJid = currentClientFullJid();
  const s = $callState.get();
  const pendingTerminate = hasPendingMucCallTerminateSession(pendingTerminates, normalized, fullJid);
  return {
    hasActiveCall: participants.length > 0 || pendingTerminate,
    selfInCall: !!nick && (participants.includes(nick) || pendingTerminate),
    localResourceInCall:
      (
        s.phase === "active" &&
        s.kind === "muc" &&
        normalizeMucCallRoomJid(s.peer) === normalized
      ) ||
      hasOwnerFullJid(participantOwners, fullJid) ||
      pendingTerminate,
    participantCount: Math.max(participants.length, pendingTerminate ? 1 : 0),
    media: mediaForRoom(normalized, s),
  };
}

function currentClientFullJid(): string {
  return fullJidIdentityKey((connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid);
}

function hasOwnerFullJid(
  owners: ReadonlyArray<{ realJid?: string }>,
  fullJid: string,
): boolean {
  if (!fullJid) return false;
  return owners.some((owner) => fullJidIdentityKey(owner.realJid) === fullJid);
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
