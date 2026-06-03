import { atom } from "nanostores";

/**
 * User-chosen audio/video device IDs, persisted to localStorage so
 * a call reconnect (or the next call entirely) picks the same
 * mic/camera/speaker without re-prompting.
 *
 * `null` means "no preference saved yet" — the engine falls back to
 * the browser's default device. We store the raw `deviceId` strings
 * from `navigator.mediaDevices.enumerateDevices()`; if a saved ID is
 * no longer present (user unplugged the headset), the engine treats
 * it as "no preference" and the browser's default takes over.
 */
type DevicePrefs = {
  mic: string | null;
  cam: string | null;
  speaker: string | null;
};

const STORAGE_KEY = "waddle:call-device-prefs";

function readInitialPrefs(): DevicePrefs {
  if (typeof window === "undefined") {
    return { mic: null, cam: null, speaker: null };
  }
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return { mic: null, cam: null, speaker: null };
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) {
      return { mic: null, cam: null, speaker: null };
    }
    const obj = parsed as Record<string, unknown>;
    return {
      mic: typeof obj.mic === "string" ? obj.mic : null,
      cam: typeof obj.cam === "string" ? obj.cam : null,
      speaker: typeof obj.speaker === "string" ? obj.speaker : null,
    };
  } catch {
    return { mic: null, cam: null, speaker: null };
  }
}

export const $devicePrefs = atom<DevicePrefs>(readInitialPrefs());

$devicePrefs.subscribe((prefs) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Persistence is best-effort.
  }
});

export function setMicDevice(id: string | null): void {
  $devicePrefs.set({ ...$devicePrefs.get(), mic: id });
}

export function setCamDevice(id: string | null): void {
  $devicePrefs.set({ ...$devicePrefs.get(), cam: id });
}

export function setSpeakerDevice(id: string | null): void {
  $devicePrefs.set({ ...$devicePrefs.get(), speaker: id });
}

/**
 * One enumerated media device — narrower than the browser's
 * `MediaDeviceInfo` because we only need `deviceId` + a label to
 * render the picker. Labels can be empty before the user has
 * granted permission; the picker falls back to a synthetic label
 * in that case.
 */
type EnumeratedDevice = {
  deviceId: string;
  kind: "audioinput" | "videoinput" | "audiooutput";
  label: string;
};

/**
 * Categorized list of devices for the settings popover. Empty arrays
 * when the user hasn't granted permission yet — the popover renders
 * an informative empty state in that case.
 */
export type EnumeratedDevices = {
  mics: EnumeratedDevice[];
  cams: EnumeratedDevice[];
  speakers: EnumeratedDevice[];
};

export async function enumerateCallDevices(): Promise<EnumeratedDevices> {
  if (typeof navigator === "undefined" || !navigator.mediaDevices?.enumerateDevices) {
    return { mics: [], cams: [], speakers: [] };
  }
  const list = await navigator.mediaDevices.enumerateDevices();
  const mics: EnumeratedDevice[] = [];
  const cams: EnumeratedDevice[] = [];
  const speakers: EnumeratedDevice[] = [];
  for (const d of list) {
    if (!d.deviceId) continue;
    const entry: EnumeratedDevice = {
      deviceId: d.deviceId,
      kind: d.kind as EnumeratedDevice["kind"],
      label: d.label,
    };
    if (d.kind === "audioinput") mics.push(entry);
    else if (d.kind === "videoinput") cams.push(entry);
    else if (d.kind === "audiooutput") speakers.push(entry);
  }
  return { mics, cams, speakers };
}
