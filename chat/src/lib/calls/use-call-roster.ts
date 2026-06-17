import { computed, type ComputedRef, type Ref } from "vue";
import { useStore } from "@nanostores/vue";
import { $callState } from "./call-store";
import { $callCamEnabled, $callMicEnabled } from "./call-controls";
import { $mucCallLiveParticipants } from "./muc-call-live-participants";
import { useCallEngine } from "./use-call-engine";
import { useCallVolumeMixer } from "./use-call-volume-mixer";
import { remoteParticipantIdentitiesForCall } from "./call-remote-identities";
import { buildCallRoster, type CallRosterRow } from "./call-roster";
import type { CallVolumeMixerRow } from "./call-volume-mixer";

/**
 * Participants-dock controller: the live roster (self + every attendee
 * with mic/camera/speaking state) plus the per-participant volume apply
 * path. The volume side is delegated to {@link useCallVolumeMixer} so the
 * remembered-levels store, the SID-change reset, and the LiveKit
 * `setParticipantAudioVolume` call all stay in one place — the dock just
 * presents those same rows grouped under their owner.
 */
export interface CallRosterController {
  rows: ComputedRef<CallRosterRow[]>;
  setVolume: (row: CallVolumeMixerRow, level: number) => void;
  resetAll: () => void;
}

export function useCallRoster(
  normalizedRoomJid: Ref<string> | ComputedRef<string>,
): CallRosterController {
  const state = useStore($callState);
  const liveParticipants = useStore($mucCallLiveParticipants);
  const micEnabled = useStore($callMicEnabled);
  const camEnabled = useStore($callCamEnabled);
  const { remoteTracks, engine, activeSpeakerIdentities } = useCallEngine();
  const volumeMixer = useCallVolumeMixer(normalizedRoomJid);

  const localIdentity = computed(() => engine.localIdentity);

  const remoteParticipantIdentities = computed(() =>
    remoteParticipantIdentitiesForCall({
      state: state.value,
      liveParticipantsByRoom: liveParticipants.value,
      normalizedRoomJid: normalizedRoomJid.value,
      remoteTracks: remoteTracks.value,
    }),
  );

  const rows = computed(() =>
    buildCallRoster({
      remoteParticipantIdentities: remoteParticipantIdentities.value,
      remoteTracks: remoteTracks.value,
      localIdentity: localIdentity.value,
      localMicEnabled: micEnabled.value,
      localCameraEnabled: camEnabled.value,
      activeSpeakerIdentities: activeSpeakerIdentities.value,
      volumeRows: volumeMixer.rows.value,
    }),
  );

  return { rows, setVolume: volumeMixer.setVolume, resetAll: volumeMixer.resetAll };
}
