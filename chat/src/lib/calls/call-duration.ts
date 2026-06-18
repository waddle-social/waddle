import { atom } from "nanostores";

/**
 * Wall-clock instant (ms epoch) at which the active call's LiveKit room
 * connected, or `null` while no call is connected. The engine stamps it
 * on the `connected` event and clears it on `disconnected`, mirroring the
 * connection-quality lifecycle. The stage-header reads it to drive a
 * live elapsed-time timer that survives split↔expanded remounts (the
 * timestamp lives in the store, not in component state).
 */
export const $callActiveSince = atom<number | null>(null);

/** Stamp the call clock at `now` (the connect instant). */
export function setCallActiveSince(now: number): void {
  $callActiveSince.set(now);
}

/** Clear the call clock when the call disconnects. */
export function resetCallActiveSince(): void {
  $callActiveSince.set(null);
}

/**
 * Elapsed milliseconds since the call clock started. `0` while the clock
 * is unset (still connecting) and clamped to `0` if `now` precedes the
 * start, so the timer never runs backwards on clock skew.
 */
export function callElapsedMs(since: number | null, now: number): number {
  if (since === null) return 0;
  return Math.max(0, now - since);
}

/**
 * Format an elapsed call duration (in milliseconds) as a compact timer
 * label: `M:SS` under an hour, `H:MM:SS` once it crosses one hour.
 * Seconds (and, in the long form, minutes) are zero-padded; the leading
 * unit is not. Pure and clock-free so the call stage-header's live timer
 * can be unit-tested without faking time.
 */
export function formatCallDuration(ms: number): string {
  const totalSeconds = Math.floor(Math.max(0, ms) / 1000);
  const seconds = totalSeconds % 60;
  const minutes = Math.floor(totalSeconds / 60) % 60;
  const hours = Math.floor(totalSeconds / 3600);
  const ss = String(seconds).padStart(2, "0");
  if (hours > 0) {
    const mm = String(minutes).padStart(2, "0");
    return `${hours}:${mm}:${ss}`;
  }
  return `${minutes}:${ss}`;
}
