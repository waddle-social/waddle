import { computed, type ComputedRef, type Ref } from "vue";
import { useStore } from "@nanostores/vue";
import { $callState } from "./call-store";
import { $callCamEnabled, $callMicEnabled } from "./call-controls";
import { $mucCallLiveParticipants } from "./muc-call-live-participants";
import {
  $mucRaisedHands,
  $selfRaisedHand,
  raisedHandKeysForRoom,
  selfRaisedHandFor,
} from "./call-raised-hand";
import { $mucMutedParticipants, mutedKeysForRoom } from "./call-mute";
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
  const { remoteTracks, activeSpeakerIdentities } = useCallEngine();
  const volumeMixer = useCallVolumeMixer(normalizedRoomJid);
  const raisedHands = useStore($mucRaisedHands);
  const selfRaisedHand = useStore($selfRaisedHand);
  const mutedParticipants = useStore($mucMutedParticipants);

  // Reactive local identity from the active call's LiveKit join, NOT the
  // engine's non-reactive `localIdentity` getter. A `computed` over the
  // getter has no reactive dependency, so it caches the pre-connect `null`
  // and never updates; once the `connected` handler seeds the live list
  // with our own identity, a stale `null` here would stop `buildCallRoster`
  // deduping self — showing the local user twice with a self volume row.
  // `join.identity` is the same identity LiveKit mints `localIdentity` from.
  const localIdentity = computed(() =>
    state.value.phase === "active" ? state.value.join.identity : null,
  );

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
      raisedHandKeys: raisedHandKeysForRoom(normalizedRoomJid.value, raisedHands.value),
      selfRaisedHand: selfRaisedHandFor(normalizedRoomJid.value, selfRaisedHand.value),
      mutedKeys: mutedKeysForRoom(normalizedRoomJid.value, mutedParticipants.value),
    }),
  );

  return { rows, setVolume: volumeMixer.setVolume, resetAll: volumeMixer.resetAll };
}
