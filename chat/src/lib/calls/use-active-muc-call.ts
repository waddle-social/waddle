import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import { $callState } from "./call-store";
import {
  $mucCallParticipants,
  normalizeMucCallRoomJid,
} from "./muc-call-presence";
import { connectionStore } from "@/lib/connection-store";

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
} {
  const state = useStore($callState);
  const participants = useStore($mucCallParticipants);

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

  return { activeRoomJid, selfInCall, participantCount };
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
} {
  const state = useStore($callState);
  const participants = useStore($mucCallParticipants);

  const normalizedRoomJid = computed<string | null>(() => {
    const normalized = normalizeMucCallRoomJid(roomJid());
    return normalized || null;
  });

  const participantList = computed<ReadonlyArray<string>>(() => {
    const room = normalizedRoomJid.value;
    if (!room) return [];
    return participants.value[room] ?? [];
  });

  const hasActiveCall = computed<boolean>(() => participantList.value.length > 0);

  const selfInCall = computed<boolean>(() => {
    const nick = connectionStore.session?.username;
    return !!nick && participantList.value.includes(nick);
  });

  const localResourceInCall = computed<boolean>(() => {
    const room = normalizedRoomJid.value;
    const s = state.value;
    if (!room || s.phase !== "active" || s.kind !== "muc") return false;
    return normalizeMucCallRoomJid(s.peer) === room;
  });

  const participantCount = computed<number>(() => participantList.value.length);

  return { hasActiveCall, selfInCall, localResourceInCall, participantCount };
}

export function readRoomHasActiveCall(roomJid: string): {
  hasActiveCall: boolean;
  selfInCall: boolean;
  localResourceInCall: boolean;
  participantCount: number;
} {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) {
    return { hasActiveCall: false, selfInCall: false, localResourceInCall: false, participantCount: 0 };
  }
  const participants = $mucCallParticipants.get()[normalized] ?? [];
  const nick = connectionStore.session?.username ?? null;
  const s = $callState.get();
  return {
    hasActiveCall: participants.length > 0,
    selfInCall: !!nick && participants.includes(nick),
    localResourceInCall:
      s.phase === "active" &&
      s.kind === "muc" &&
      normalizeMucCallRoomJid(s.peer) === normalized,
    participantCount: participants.length,
  };
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
} {
  const s = $callState.get();
  if (s.phase !== "active" || s.kind !== "muc") {
    return { activeRoomJid: null, selfInCall: false, participantCount: 0 };
  }
  const roomJid = normalizeMucCallRoomJid(s.peer) || null;
  if (!roomJid) {
    return { activeRoomJid: null, selfInCall: false, participantCount: 0 };
  }
  const nicks = $mucCallParticipants.get()[roomJid] ?? [];
  const nick = connectionStore.session?.username ?? null;
  return {
    activeRoomJid: roomJid,
    selfInCall: nick !== null && nicks.includes(nick),
    participantCount: nicks.length,
  };
}
