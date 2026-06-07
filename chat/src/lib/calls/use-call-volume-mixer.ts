import { computed, type ComputedRef, type Ref } from "vue";
import { atom } from "nanostores";
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

type CallVolumeMixerTarget = {
  participantIdentity: string;
  source: CallVolumeMixerRow["source"];
};

/**
 * Shared "Who you hear" mixer controller for the in-call surfaces
 * (split + expanded). Both surfaces reach the mixer from the control
 * bar's speaker button (#909), so the row projection and the LiveKit
 * `setParticipantAudioVolume` apply path live here once instead of
 * being duplicated per surface.
 *
 * The remembered levels are module-scoped, not per-component: split
 * and expanded are two views of ONE call, so a gain set in one must
 * read back identically in the other. Holding the state in shared
 * nanostores keeps that single source of truth (and stays resettable
 * in tests). Levels are keyed per LiveKit participant+source so a
 * participant who mutes — dropping their audio track — keeps the
 * volume the user last chose, ready to re-apply when they unmute.
 */
const $rememberedLevels = atom<CallVolumeLevelStore>({});
const $rememberedTargets = atom<Record<string, CallVolumeMixerTarget>>({});

// Wipe remembered state whenever the active call's SID changes so one
// call's preferences never bleed into the next. Subscribed once at
// module load, independent of how many surfaces are mounted.
let lastActiveSid: string | null = null;
$callState.subscribe((state) => {
  const sid = state.phase === "active" ? state.sid : null;
  if (sid === lastActiveSid) return;
  lastActiveSid = sid;
  $rememberedLevels.set({});
  $rememberedTargets.set({});
});

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
  const rememberedLevels = useStore($rememberedLevels);
  const { remoteTracks, engine } = useCallEngine();

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
      levels: rememberedLevels.value,
    }),
  );

  function setVolume(row: CallVolumeMixerRow, level: number): void {
    $rememberedLevels.set({ ...$rememberedLevels.get(), [row.key]: level });
    $rememberedTargets.set({
      ...$rememberedTargets.get(),
      [row.key]: {
        participantIdentity: row.participantIdentity,
        source: row.source,
      },
    });
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
    // every audible participant at unity gain. Rows already covered by
    // a remembered target are skipped so each participant is reset once.
    const touched = $rememberedTargets.get();
    for (const target of Object.values(touched)) {
      engine.setParticipantAudioVolume({
        participantIdentity: target.participantIdentity,
        source: target.source,
        volume: 1,
      });
    }
    for (const row of rows.value) {
      if (touched[row.key]) continue;
      engine.setParticipantAudioVolume({
        participantIdentity: row.participantIdentity,
        source: row.source,
        volume: 1,
      });
    }
    $rememberedLevels.set(resetCallVolumeMixerLevels($rememberedLevels.get()));
  }

  return { rows, setVolume, resetAll };
}
