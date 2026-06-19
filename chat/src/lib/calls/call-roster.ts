import type { RemoteMediaTrack } from "./engine";
import type { CallVolumeMixerRow } from "./call-volume-mixer";
import { fullJidIdentityKey } from "@/lib/xmpp/jid";

/**
 * One attendee in the call's Participants roster. A self-contained row
 * the dock renders directly: live mic/camera state, a speaking flag for
 * the highlight, and the per-participant volume controls grouped from
 * the shared "Who you hear" mixer rows (empty for self — you cannot turn
 * your own volume down).
 */
export type CallRosterRow = {
  key: string;
  identity: string;
  label: string;
  isSelf: boolean;
  micOn: boolean;
  cameraOn: boolean;
  speaking: boolean;
  /** True when this attendee advertises a raised hand (#1029). */
  raisedHand: boolean;
  volumeRows: CallVolumeMixerRow[];
};

export type BuildCallRosterInput = {
  remoteParticipantIdentities: readonly string[];
  remoteTracks: readonly RemoteMediaTrack[];
  localIdentity: string | null;
  localMicEnabled: boolean;
  localCameraEnabled: boolean;
  activeSpeakerIdentities: ReadonlySet<string>;
  volumeRows: readonly CallVolumeMixerRow[];
  /**
   * Identity keys (`fullJidIdentityKey`) of remote attendees with a
   * raised hand (#1029). Defaults to empty.
   */
  raisedHandKeys?: ReadonlySet<string>;
  /** Whether this client currently advertises a raised hand. */
  selfRaisedHand?: boolean;
};

type MediaState = { micOn: boolean; cameraOn: boolean };

export function buildCallRoster(input: BuildCallRosterInput): CallRosterRow[] {
  const selfKey = fullJidIdentityKey(input.localIdentity);

  const speakingKeys = new Set<string>();
  for (const identity of input.activeSpeakerIdentities) {
    const key = fullJidIdentityKey(identity);
    if (key) speakingKeys.add(key);
  }

  const volumeRowsByKey = new Map<string, CallVolumeMixerRow[]>();
  for (const volume of input.volumeRows) {
    const key = fullJidIdentityKey(volume.participantIdentity);
    if (!key || key === selfKey) continue;
    const existing = volumeRowsByKey.get(key);
    if (existing) existing.push(volume);
    else volumeRowsByKey.set(key, [volume]);
  }

  const mediaByKey = new Map<string, MediaState>();
  for (const track of input.remoteTracks) {
    const key = fullJidIdentityKey(track.participantIdentity);
    if (!key || key === selfKey) continue;
    const media = mediaByKey.get(key) ?? { micOn: false, cameraOn: false };
    if (track.kind === "audio" && track.source === "microphone") media.micOn = true;
    if (track.kind === "video" && track.source === "camera") media.cameraOn = true;
    mediaByKey.set(key, media);
  }

  // When we have no local identity yet (pre-connect), the self row still
  // needs a stable key; fall back to "self". The dedup set is seeded with
  // this same key so a remote can never collide with the self row.
  const selfRowKey = selfKey || "self";

  const raisedHandKeys = input.raisedHandKeys ?? new Set<string>();

  const rows: CallRosterRow[] = [];
  rows.push({
    key: selfRowKey,
    identity: input.localIdentity ?? "you",
    label: "You",
    isSelf: true,
    micOn: input.localMicEnabled,
    cameraOn: input.localCameraEnabled,
    speaking: speakingKeys.has(selfKey),
    raisedHand: input.selfRaisedHand === true || raisedHandKeys.has(selfKey),
    volumeRows: [],
  });

  // The roster list is authoritative for order; a participant present
  // only via a published track (e.g. a 1:1 peer before the presence list
  // catches up) is still an attendee, appended after the listed ones.
  const seen = new Set<string>([selfRowKey]);
  const orderedIdentities = [
    ...input.remoteParticipantIdentities,
    ...input.remoteTracks.map((track) => track.participantIdentity),
  ];
  for (const identity of orderedIdentities) {
    const key = fullJidIdentityKey(identity);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    const media = mediaByKey.get(key) ?? { micOn: false, cameraOn: false };
    rows.push({
      key,
      identity,
      label: displayNameForIdentity(identity),
      isSelf: false,
      micOn: media.micOn,
      cameraOn: media.cameraOn,
      speaking: speakingKeys.has(key),
      raisedHand: raisedHandKeys.has(key),
      volumeRows: volumeRowsByKey.get(key) ?? [],
    });
  }

  return rows;
}

function displayNameForIdentity(identity: string): string {
  const at = identity.indexOf("@");
  if (at <= 0) return identity;
  return identity.slice(0, at);
}
