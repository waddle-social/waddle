import type { RemoteMediaTrack } from "./engine";
import { fullJidIdentityKey } from "@/lib/xmpp/jid";

type CallVolumeMixerSource = "microphone" | "screen_share_audio";

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

function callVolumeMixerKey(
  participantIdentity: string,
  source: CallVolumeMixerSource,
): string {
  return `${participantIdentity}:${source}`;
}

export function buildCallVolumeMixerRows(
  input: BuildCallVolumeMixerRowsInput,
): CallVolumeMixerRow[] {
  const localIdentity = normalizedIdentityKey(input.localIdentity);
  const remoteParticipants = new Map<string, string>();
  const liveAudioIdentities = new Map<string, string>();
  const liveAudioSources = new Set<string>();

  for (const identity of input.remoteParticipantIdentities) {
    const participantKey = normalizedIdentityKey(identity);
    if (!participantKey || participantKey === localIdentity) continue;
    remoteParticipants.set(participantKey, identity);
  }

  for (const track of input.remoteTracks) {
    const participantKey = normalizedIdentityKey(track.participantIdentity);
    if (!participantKey || participantKey === localIdentity) continue;
    if (!isMixerAudioSource(track.source)) continue;
    if (!remoteParticipants.has(participantKey)) {
      remoteParticipants.set(participantKey, track.participantIdentity);
    }
    liveAudioIdentities.set(callVolumeMixerKey(participantKey, track.source), track.participantIdentity);
    liveAudioSources.add(callVolumeMixerKey(participantKey, track.source));
  }

  const rows: CallVolumeMixerRow[] = [];
  for (const [participantKey, fallbackIdentity] of remoteParticipants) {
    const voiceIdentity = liveAudioIdentities.get(callVolumeMixerKey(participantKey, "microphone"))
      ?? fallbackIdentity;
    const voiceKey = callVolumeMixerKey(participantKey, "microphone");
    const voiceLevel = input.levels[voiceKey] ?? 1;
    const voiceLabel = displayNameForIdentity(voiceIdentity);
    const voiceDisabled = !liveAudioSources.has(callVolumeMixerKey(participantKey, "microphone"));
    rows.push({
      key: voiceKey,
      participantIdentity: voiceIdentity,
      source: "microphone",
      label: voiceLabel,
      level: voiceLevel,
      disabled: voiceDisabled,
      hint: voiceDisabled ? "mic off" : null,
      muted: !voiceDisabled && voiceLevel === 0,
      ariaLabel: `Volume for ${voiceLabel}`,
      ariaValueText: `${Math.round(voiceLevel * 100)}%`,
    });

    const screenSourceKey = callVolumeMixerKey(participantKey, "screen_share_audio");
    if (!liveAudioSources.has(screenSourceKey)) continue;
    const screenIdentity = liveAudioIdentities.get(screenSourceKey) ?? fallbackIdentity;
    const screenKey = screenSourceKey;
    const screenLevel = input.levels[screenKey] ?? 1;
    const screenLabel = `${voiceLabel}'s screen`;
    rows.push({
      key: screenKey,
      participantIdentity: screenIdentity,
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

function normalizedIdentityKey(identity: string | null | undefined): string {
  return fullJidIdentityKey(identity);
}

function isMixerAudioSource(source: string): source is CallVolumeMixerSource {
  return source === "microphone" || source === "screen_share_audio";
}
