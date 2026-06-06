import { atom } from "nanostores";

export const $callAudioPlaybackBlocked = atom(false);

export type CallAudioResumeTarget = {
  startAudio(): Promise<unknown>;
};

export function setCallAudioPlaybackBlocked(blocked: boolean): void {
  $callAudioPlaybackBlocked.set(blocked);
}

export async function resumeCallAudioPlayback(
  target: CallAudioResumeTarget,
  onFailure: () => void,
): Promise<void> {
  try {
    await target.startAudio();
  } catch {
    onFailure();
  }
}
