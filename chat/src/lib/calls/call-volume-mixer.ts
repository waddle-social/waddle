import type { RemoteMediaTrack } from "./engine";

export type CallVolumeMixerSource = "microphone" | "screen_share_audio";

export type CallVolumeLevelStore = Record<string, number>;

export type CallVolumeMixerRow = {
  key: string;
  participantIdentity: string;
  source: CallVolumeMixerSource;
  label: string;
  level: number;
  disabled: boolean;
  hint: string | null;
  muted: boolean;
  ariaLabel: string;
  ariaValueText: string;
};

export type BuildCallVolumeMixerRowsInput = {
  remoteParticipantIdentities: readonly string[];
  remoteTracks: readonly RemoteMediaTrack[];
  localIdentity: string | null;
  levels: CallVolumeLevelStore;
};

export function callVolumeMixerKey(
  participantIdentity: string,
  source: CallVolumeMixerSource,
): string {
  return `${participantIdentity}:${source}`;
}

export function buildCallVolumeMixerRows(
  input: BuildCallVolumeMixerRowsInput,
): CallVolumeMixerRow[] {
  const localIdentity = input.localIdentity?.toLowerCase() ?? null;
  const remoteIdentities = new Set<string>();
  const liveAudioSources = new Set<string>();

  for (const identity of input.remoteParticipantIdentities) {
    if (isSelfIdentity(identity, localIdentity)) continue;
    remoteIdentities.add(identity);
  }

  for (const track of input.remoteTracks) {
    if (isSelfIdentity(track.participantIdentity, localIdentity)) continue;
    if (!isMixerAudioSource(track.source)) continue;
    remoteIdentities.add(track.participantIdentity);
    liveAudioSources.add(callVolumeMixerKey(track.participantIdentity, track.source));
  }

  const rows: CallVolumeMixerRow[] = [];
  for (const participantIdentity of remoteIdentities) {
    const voiceKey = callVolumeMixerKey(participantIdentity, "microphone");
    const voiceLevel = input.levels[voiceKey] ?? 1;
    const voiceLabel = displayNameForIdentity(participantIdentity);
    const voiceDisabled = !liveAudioSources.has(voiceKey);
    rows.push({
      key: voiceKey,
      participantIdentity,
      source: "microphone",
      label: voiceLabel,
      level: voiceLevel,
      disabled: voiceDisabled,
      hint: voiceDisabled ? "mic off" : null,
      muted: !voiceDisabled && voiceLevel === 0,
      ariaLabel: `Volume for ${voiceLabel}`,
      ariaValueText: `${Math.round(voiceLevel * 100)}%`,
    });

    const screenKey = callVolumeMixerKey(participantIdentity, "screen_share_audio");
    if (!liveAudioSources.has(screenKey)) continue;
    const screenLevel = input.levels[screenKey] ?? 1;
    const screenLabel = `${voiceLabel}'s screen`;
    rows.push({
      key: screenKey,
      participantIdentity,
      source: "screen_share_audio",
      label: screenLabel,
      level: screenLevel,
      disabled: false,
      hint: null,
      muted: screenLevel === 0,
      ariaLabel: `Volume for ${screenLabel}`,
      ariaValueText: `${Math.round(screenLevel * 100)}%`,
    });
  }

  return rows;
}

export function resetCallVolumeMixerLevels(
  levels: CallVolumeLevelStore,
): CallVolumeLevelStore {
  return Object.fromEntries(Object.keys(levels).map((key) => [key, 1]));
}

function displayNameForIdentity(identity: string): string {
  const at = identity.indexOf("@");
  if (at <= 0) return identity;
  return identity.slice(0, at);
}

function isSelfIdentity(identity: string, localIdentity: string | null): boolean {
  return localIdentity !== null && identity.toLowerCase() === localIdentity;
}

function isMixerAudioSource(source: string): source is CallVolumeMixerSource {
  return source === "microphone" || source === "screen_share_audio";
}
