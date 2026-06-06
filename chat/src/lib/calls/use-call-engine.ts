import { ref, type Ref } from "vue";
import { $callState } from "./call-store";
import { CallEngine, type LocalMediaTrack, type RemoteMediaTrack } from "./engine";
import {
  addLiveCallParticipant,
  clearLiveCallParticipants,
  markRoomLeavingCall,
  removeLiveCallParticipant,
  setLiveCallParticipants,
} from "./muc-call-live-participants";
import { clearAllMediaIssues, recordMediaIssue } from "./call-media-issues";
import { syncScreenShareEnabled } from "./screen-share-state";

/**
 * Process-wide singleton: only one call engine should ever exist
 * because the call-store only tracks one call at a time. The Vue
 * tree may mount/unmount the overlay component repeatedly during a
 * call; tying the engine to the component lifecycle would tear it
 * down on every re-render, so the engine outlives the component.
 */
let singletonEngine: CallEngine | null = null;
const remoteTracks: Ref<RemoteMediaTrack[]> = ref([]);
/**
 * Local tracks the user has currently published (camera, mic, …).
 * Drives the self-preview tile in `CallOverlay`. Without this the
 * user only sees other participants — the most common call-UI bug
 * report ("can't see my own camera"). Symmetric with `remoteTracks`
 * so the renderer can use one tile component for both.
 */
const localTracks: Ref<LocalMediaTrack[]> = ref([]);

export function useCallEngine(): {
  engine: CallEngine;
  remoteTracks: Ref<RemoteMediaTrack[]>;
  localTracks: Ref<LocalMediaTrack[]>;
} {
  if (!singletonEngine) {
    singletonEngine = new CallEngine();
    singletonEngine.on("trackSubscribed", (track) => {
      remoteTracks.value = [...remoteTracks.value, track];
    });
    singletonEngine.on("trackUnsubscribed", (track) => {
      remoteTracks.value = remoteTracks.value.filter(
        (existing) => existing.publicationSid !== track.publicationSid,
      );
    });
    singletonEngine.on("localTrackPublished", (track) => {
      localTracks.value = [...localTracks.value, track];
      if (track.source === "screen_share") syncScreenShareEnabled(true);
    });
    singletonEngine.on("localTrackUnpublished", (track) => {
      localTracks.value = localTracks.value.filter(
        (existing) => existing.publicationSid !== track.publicationSid,
      );
      if (track.source === "screen_share") syncScreenShareEnabled(false);
    });
    singletonEngine.on("connected", ({ localIdentity, remoteIdentities }) => {
      // Seed the LK-truth projection for the active MUC room with
      // the initial remote-participant snapshot LK gave us at
      // connect time. Local identity is included so the room view
      // shows ourselves alongside peers without waiting for the
      // (already-true) Muji presence to also broadcast.
      const roomJid = activeMucRoomJid();
      if (!roomJid) return;
      setLiveCallParticipants(roomJid, [localIdentity, ...remoteIdentities]);
    });
    singletonEngine.on("mediaDevicesError", ({ source, error }) => {
      // A best-effort mic/cam capture failed (no device / denied
      // permission). The call stays connected as receive-only; record
      // the issue so the non-blocking notice can explain it and offer
      // a retry. NOT routed through reportCallError — the persistent
      // notice is the single channel, avoiding a double-surfaced error.
      recordMediaIssue(source === "audio" ? "mic" : "cam", error);
    });
    singletonEngine.on("participantConnected", (identity) => {
      const roomJid = activeMucRoomJid();
      if (!roomJid) return;
      addLiveCallParticipant(roomJid, identity);
    });
    singletonEngine.on("participantDisconnected", (identity) => {
      const roomJid = activeMucRoomJid();
      if (!roomJid) return;
      removeLiveCallParticipant(roomJid, identity);
    });
    singletonEngine.on("disconnected", () => {
      remoteTracks.value = [];
      localTracks.value = [];
      syncScreenShareEnabled(false);
      // Drop any "joined without mic/camera" notice — it belongs to the
      // call that just ended, not the next one.
      clearAllMediaIssues();
      // The active call's room — if any — is no longer in our LK
      // view. Drop the LK snapshot so the room's UI reverts to the
      // Muji-derived (server-bridged) view, which the LK webhook
      // bridge keeps honest within seconds.
      const slot = activeMucCallSlot();
      if (!slot) return;
      // Suppress our own stale Muji nick for one render cycle: the
      // server-authoritative `<muji/>` absence broadcast arrives ~1
      // RTT after LK fires `disconnected`, so without this marker the
      // resolved participant list would fall back to a Muji view that
      // still lists us — bouncing the indicator 0 → N → 0.
      markRoomLeavingCall(slot.roomJid, slot.selfNick);
      clearLiveCallParticipants(slot.roomJid);
    });
  }
  return { engine: singletonEngine, remoteTracks, localTracks };
}

/**
 * Read the bare room JID of the MUC call our `$callState` slot is
 * currently bound to, or `null` when no MUC call is active. The LK
 * projection key on this so a 1:1 DM call's engine events do not
 * leak into the MUC participant store.
 */
function activeMucRoomJid(): string | null {
  return activeMucCallSlot()?.roomJid ?? null;
}

/**
 * Read the active MUC call's room JID together with our own MUC nick,
 * or `null` when no MUC call is bound to the slot. The nick lets the
 * teardown path suppress our own stale Muji presence until the server
 * broadcasts our `<muji/>` absence.
 */
function activeMucCallSlot(): { roomJid: string; selfNick: string | null } | null {
  const state = $callState.get();
  if (state.phase !== "active" && state.phase !== "muc-pending") return null;
  if (state.kind !== "muc") return null;
  return { roomJid: state.peer, selfNick: state.selfNick ?? null };
}
