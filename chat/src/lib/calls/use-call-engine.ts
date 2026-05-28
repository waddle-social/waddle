import { ref, type Ref } from "vue";
import { $callState } from "./call-store";
import { CallEngine, type LocalMediaTrack, type RemoteMediaTrack } from "./engine";
import {
  addLiveCallParticipant,
  clearLiveCallParticipants,
  removeLiveCallParticipant,
  setLiveCallParticipants,
} from "./muc-call-live-participants";

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
    });
    singletonEngine.on("localTrackUnpublished", (track) => {
      localTracks.value = localTracks.value.filter(
        (existing) => existing.publicationSid !== track.publicationSid,
      );
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
      // The active call's room — if any — is no longer in our LK
      // view. Drop the LK snapshot so the room's UI reverts to the
      // Muji-derived (server-bridged) view, which the LK webhook
      // bridge keeps honest within seconds.
      const roomJid = activeMucRoomJid();
      if (roomJid) clearLiveCallParticipants(roomJid);
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
  const state = $callState.get();
  if (state.phase !== "active" && state.phase !== "muc-pending") return null;
  if (state.kind !== "muc") return null;
  return state.peer;
}
