import type { RemoteMediaTrack } from "./engine";
import type { CallState } from "./types";

/**
 * The remote (non-self) participant identities for the active call: the
 * live Muji/LiveKit roster for a MUC, or the DM peer plus any identity we
 * already have a remote track for. Shared by the volume mixer and the
 * Participants roster so both project the same attendee set from one rule.
 */
export function remoteParticipantIdentitiesForCall(input: {
  state: CallState;
  liveParticipantsByRoom: Readonly<Record<string, readonly string[]>>;
  normalizedRoomJid: string;
  remoteTracks: readonly RemoteMediaTrack[];
}): readonly string[] {
  if (input.state.phase === "active" && input.state.kind === "muc") {
    return input.liveParticipantsByRoom[input.normalizedRoomJid] ?? [];
  }
  const identities = new Set<string>();
  if (input.state.phase === "active" && input.state.kind === "dm") {
    identities.add(input.state.peer);
  }
  for (const track of input.remoteTracks) identities.add(track.participantIdentity);
  return Array.from(identities);
}
