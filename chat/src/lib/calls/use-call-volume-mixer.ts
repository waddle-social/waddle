import { computed, ref, watch, type ComputedRef, type Ref } from "vue";
import { useStore } from "@nanostores/vue";
import { $callState } from "./call-store";
import { $mucCallLiveParticipants } from "./muc-call-live-participants";
import { useCallEngine } from "./use-call-engine";
import {
  buildCallVolumeMixerRows,
  resetCallVolumeMixerLevels,
  type CallVolumeLevelStore,
  type CallVolumeMixerRow,
} from "./call-volume-mixer";

/**
 * Shared "Who you hear" mixer controller for the in-call surfaces
 * (split + expanded). Both surfaces reach the mixer from the control
 * bar's speaker button (#909), so the row projection, remembered
 * levels, and the LiveKit `setParticipantAudioVolume` apply path live
 * here once instead of being duplicated per surface.
 *
 * `remembered levels` are kept per LiveKit participant+source key so a
 * participant who mutes (dropping their audio track) keeps the volume
 * the user last set, ready to re-apply when they unmute. The remembered
 * state is wiped whenever the active call's SID changes so one call's
 * preferences never bleed into the next.
 */
export interface CallVolumeMixerController {
  rows: ComputedRef<CallVolumeMixerRow[]>;
  setVolume: (row: CallVolumeMixerRow, level: number) => void;
  resetAll: () => void;
}

export function useCallVolumeMixer(
  normalizedRoomJid: Ref<string> | ComputedRef<string>,
): CallVolumeMixerController {
  const state = useStore($callState);
  const liveParticipants = useStore($mucCallLiveParticipants);
  const { remoteTracks, engine } = useCallEngine();

  const participantAudioLevels = ref<CallVolumeLevelStore>({});
  const participantAudioTargets = ref<Record<string, {
    participantIdentity: string;
    source: CallVolumeMixerRow["source"];
  }>>({});

  const localIdentity = computed(() => engine.localIdentity);

  const remoteParticipantIdentities = computed(() => {
    if (state.value.phase === "active" && state.value.kind === "muc") {
      return liveParticipants.value[normalizedRoomJid.value] ?? [];
    }
    const identities = new Set<string>();
    if (state.value.phase === "active" && state.value.kind === "dm") {
      identities.add(state.value.peer);
    }
    for (const track of remoteTracks.value) identities.add(track.participantIdentity);
    return Array.from(identities);
  });

  const rows = computed(() =>
    buildCallVolumeMixerRows({
      remoteParticipantIdentities: remoteParticipantIdentities.value,
      remoteTracks: remoteTracks.value,
      localIdentity: localIdentity.value,
      levels: participantAudioLevels.value,
    }),
  );

  const activeCallSid = computed(() =>
    state.value.phase === "active" ? state.value.sid : null,
  );

  function setVolume(row: CallVolumeMixerRow, level: number): void {
    participantAudioLevels.value = {
      ...participantAudioLevels.value,
      [row.key]: level,
    };
    participantAudioTargets.value = {
      ...participantAudioTargets.value,
      [row.key]: {
        participantIdentity: row.participantIdentity,
        source: row.source,
      },
    };
    engine.setParticipantAudioVolume({
      participantIdentity: row.participantIdentity,
      source: row.source,
      volume: level,
    });
  }

  function resetAll(): void {
    // Reset both the targets the user has touched this call AND the
    // currently-projected rows: a participant can be a remembered
    // target without a live row (muted) or a live row without a
    // remembered target (default volume), and "Reset all" must land
    // every audible participant at unity gain.
    for (const target of Object.values(participantAudioTargets.value)) {
      engine.setParticipantAudioVolume({
        participantIdentity: target.participantIdentity,
        source: target.source,
        volume: 1,
      });
    }
    for (const row of rows.value) {
      engine.setParticipantAudioVolume({
        participantIdentity: row.participantIdentity,
        source: row.source,
        volume: 1,
      });
    }
    participantAudioLevels.value = resetCallVolumeMixerLevels(participantAudioLevels.value);
  }

  watch(activeCallSid, () => {
    participantAudioLevels.value = {};
    participantAudioTargets.value = {};
  });

  return { rows, setVolume, resetAll };
}
