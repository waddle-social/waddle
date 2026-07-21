import { atom } from "nanostores";
import { reportCallMediaError } from "../telemetry";

/**
 * Why the local microphone or camera could not be published for the
 * active call. Distinct from a *deliberate* mute: muting yourself
 * leaves no issue recorded, while a missing device or a denied
 * permission does. Drives the non-blocking "joined without a
 * microphone/camera" notice (see `CallMediaNotice.vue`).
 *
 *  - `denied`  — the browser blocked capture (`NotAllowedError`).
 *  - `missing` — no device of that kind is available (`NotFoundError`).
 *  - `in-use`  — a device exists but another app holds it
 *                (`NotReadableError` / `AbortError`).
 *  - `failed`  — capture failed for some other reason.
 */
export type MediaIssueReason = "denied" | "missing" | "in-use" | "failed";

export type MediaKind = "mic" | "cam" | "screen";

type CallMediaIssues = {
  mic: MediaIssueReason | null;
  cam: MediaIssueReason | null;
  screen: MediaIssueReason | null;
};

/**
 * Per-kind capture problems for the active call. `null` means the kind
 * is fine — either publishing, or deliberately muted with no error.
 * Set when a best-effort capture at join time (or a later mic/cam
 * toggle) fails; cleared when the kind successfully (re)publishes or
 * when the call ends.
 */
export const $callMediaIssues = atom<CallMediaIssues>({
  mic: null,
  cam: null,
  screen: null,
});

/**
 * Read the `name` of a `getUserMedia` / LiveKit capture rejection.
 * LiveKit re-throws the underlying `DOMException`, so `.name` is the
 * reliable discriminator. Returns `null` when the value isn't an
 * error-with-name (so callers can treat it as "not a media error").
 */
function mediaErrorName(error: unknown): string | null {
  if (error && typeof error === "object" && "name" in error) {
    const name = (error as { name?: unknown }).name;
    if (typeof name === "string" && name.length > 0) return name;
  }
  return null;
}

/**
 * Known `getUserMedia` / LiveKit capture rejection names mapped to a
 * coarse reason. Browsers disagree on `DOMException` names (Chrome
 * historically used `PermissionDeniedError`, `DevicesNotFoundError`,
 * `TrackStartError`, …), so we list the standard spec names plus the
 * known legacy aliases. A name absent from this map is NOT recognized
 * as a media error.
 */
const MEDIA_ERROR_REASONS: Readonly<Record<string, MediaIssueReason>> = {
  NotAllowedError: "denied",
  PermissionDeniedError: "denied",
  PermissionDismissedError: "denied",
  SecurityError: "denied",
  NotFoundError: "missing",
  DevicesNotFoundError: "missing",
  OverconstrainedError: "missing",
  ConstraintNotSatisfiedError: "missing",
  NotReadableError: "in-use",
  TrackStartError: "in-use",
  AbortError: "in-use",
};

/**
 * Map a capture rejection to a coarse [`MediaIssueReason`]. Use this
 * ONLY for errors already known to come from a capture attempt (the
 * engine's mic/cam enable, a mic/cam toggle): an unrecognized name
 * falls back to `failed` because the failure IS a capture failure,
 * just with an unfamiliar name.
 */
export function classifyMediaError(error: unknown): MediaIssueReason {
  const name = mediaErrorName(error);
  return (name && MEDIA_ERROR_REASONS[name]) || "failed";
}

const MIC_COPY: Record<MediaIssueReason, string> = {
  denied:
    "Microphone access is blocked — you joined as a listener. Allow access in your browser, then enable your mic.",
  missing: "No microphone detected — you joined as a listener. Connect one, then enable your mic.",
  "in-use":
    "Your microphone is in use by another app. Close it, then enable your mic.",
  failed: "Couldn't start your microphone — you joined as a listener.",
};

const CAM_COPY: Record<MediaIssueReason, string> = {
  denied:
    "Camera access is blocked — you joined without video. Allow access in your browser, then turn your camera on.",
  missing: "No camera detected — you joined without video. Connect one, then turn your camera on.",
  "in-use":
    "Your camera is in use by another app. Close it, then turn your camera on.",
  failed: "Couldn't start your camera — you joined without video.",
};

const SCREEN_COPY: Record<MediaIssueReason, string> = {
  denied: "Screen sharing was blocked. Allow access in your browser, then share again.",
  missing: "No screen capture source was available.",
  "in-use": "Couldn't start screen sharing. Try a different window or tab.",
  failed: "Couldn't start screen sharing.",
};

/** Human-facing copy for a recorded issue, shown in the notice banner. */
export function mediaIssueMessage(kind: MediaKind, reason: MediaIssueReason): string {
  if (kind === "mic") return MIC_COPY[reason];
  if (kind === "cam") return CAM_COPY[reason];
  return SCREEN_COPY[reason];
}

const GENERIC_MEDIA_COPY: Readonly<Record<MediaIssueReason, string>> = {
  denied: "Camera/microphone access is blocked. Allow it in your browser to publish.",
  missing: "No camera or microphone detected.",
  "in-use": "Your camera or microphone is in use by another app.",
  failed: "Couldn't start your camera or microphone.",
};

/**
 * Friendly, kind-neutral one-liner for a RECOGNIZED capture error
 * surfaced through the generic error toast (`reportCallError`).
 * Returns `null` for anything that isn't a known media `DOMException`
 * — including plain `Error`s (name `"Error"`) and XMPP errors — so the
 * caller falls back to the raw message instead of mislabeling them.
 */
export function mediaErrorMessage(error: unknown): string | null {
  const name = mediaErrorName(error);
  if (name === null) return null;
  const reason = MEDIA_ERROR_REASONS[name];
  return reason ? GENERIC_MEDIA_COPY[reason] : null;
}

/** Record a capture failure for `kind`, classifying the error. */
export function recordMediaIssue(kind: MediaKind, error: unknown): void {
  const reason = classifyMediaError(error);
  $callMediaIssues.set({ ...$callMediaIssues.get(), [kind]: reason });
  reportCallMediaError(kind, reason);
}

/** Clear the recorded issue for `kind` (e.g. after a successful retry). */
export function clearMediaIssue(kind: MediaKind): void {
  if ($callMediaIssues.get()[kind] === null) return;
  $callMediaIssues.set({ ...$callMediaIssues.get(), [kind]: null });
}

/** Drop all recorded issues — used at call start and on disconnect. */
export function clearAllMediaIssues(): void {
  const current = $callMediaIssues.get();
  if (current.mic === null && current.cam === null && current.screen === null) return;
  $callMediaIssues.set({ mic: null, cam: null, screen: null });
}
