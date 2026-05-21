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
 * - `CallSplitContainer` (renders only when the viewed channel owns
 *   the call AND the local user is a participant).
 * - `CallActivePill` (hides itself when the local user has joined).
 * - `ChatHeader` indicator wiring.
 *
 * The selector intentionally treats DM calls (`kind: "dm"`) as
 * out-of-scope: the split-view surface only ever covers MUC group
 * calls because that's where the channel-vs-call ownership question
 * exists. DM calls keep their toast-based UX.
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
