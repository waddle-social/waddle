import { atom } from "nanostores";
import { isNoiseModelId, type NoiseModelId } from "./ai-noise-filter/model-id";
import { hasNoiseModelBackend } from "./ai-noise-filter/registry";
import {
  BACKGROUND_OFF,
  normalizeBackgroundEffect,
  type BackgroundEffect,
} from "./background-effect/effect-id";

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
  audioProcessing: AudioProcessingPrefs;
  /**
   * The opt-in client-side AI noise model, or `null` for off (the default).
   * Separate from `audioProcessing` because it is a different mechanism (a
   * WASM `TrackProcessor`, not a `getUserMedia` constraint) with a different
   * lifecycle and a different default.
   */
  aiNoiseModel: NoiseModelId | null;
  /**
   * The opt-in camera background effect (off / blur / image), default off.
   * Re-applied at the next call's camera publish. A custom image's `ref` points
   * at bytes in the custom-image store; the catalog `id` at a bundled asset.
   */
  backgroundEffect: BackgroundEffect;
};

const STORAGE_KEY = "waddle:call-device-prefs";

export type AudioProcessingPrefs = {
  noiseSuppression: boolean;
  echoCancellation: boolean;
  autoGainControl: boolean;
};

export type AudioProcessingConstraints = AudioProcessingPrefs;

export function defaultAudioProcessingPrefs(): AudioProcessingPrefs {
  return {
    noiseSuppression: true,
    echoCancellation: true,
    autoGainControl: true,
  };
}

export function normalizeAudioProcessingPrefs(value: unknown): AudioProcessingPrefs {
  if (typeof value !== "object" || value === null) return defaultAudioProcessingPrefs();
  const obj = value as Record<string, unknown>;
  if (
    typeof obj.noiseSuppression !== "boolean" ||
    typeof obj.echoCancellation !== "boolean" ||
    typeof obj.autoGainControl !== "boolean"
  ) {
    return defaultAudioProcessingPrefs();
  }
  return {
    noiseSuppression: obj.noiseSuppression,
    echoCancellation: obj.echoCancellation,
    autoGainControl: obj.autoGainControl,
  };
}

export function audioProcessingConstraints(
  prefs: AudioProcessingPrefs,
): AudioProcessingConstraints {
  return {
    noiseSuppression: prefs.noiseSuppression,
    echoCancellation: prefs.echoCancellation,
    autoGainControl: prefs.autoGainControl,
  };
}

function defaultDevicePrefs(): DevicePrefs {
  return {
    mic: null,
    cam: null,
    speaker: null,
    audioProcessing: defaultAudioProcessingPrefs(),
    aiNoiseModel: null,
    backgroundEffect: BACKGROUND_OFF,
  };
}

/**
 * Narrow a persisted `aiNoiseModel` to a known model that actually ships a
 * backend, else `null` (off). A deferred/unimplemented model (e.g. the
 * disabled `deepfilternet` slot) normalizes to off so the engine never tries —
 * and perpetually fails — to attach something with no loader.
 */
function normalizeAiNoiseModel(value: unknown): NoiseModelId | null {
  return isNoiseModelId(value) && hasNoiseModelBackend(value) ? value : null;
}

export function parseDevicePrefsStorage(raw: string | null): DevicePrefs {
  try {
    if (!raw) return defaultDevicePrefs();
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) {
      return defaultDevicePrefs();
    }
    const obj = parsed as Record<string, unknown>;
    return {
      mic: typeof obj.mic === "string" ? obj.mic : null,
      cam: typeof obj.cam === "string" ? obj.cam : null,
      speaker: typeof obj.speaker === "string" ? obj.speaker : null,
      audioProcessing: normalizeAudioProcessingPrefs(obj.audioProcessing),
      aiNoiseModel: normalizeAiNoiseModel(obj.aiNoiseModel),
      backgroundEffect: normalizeBackgroundEffect(obj.backgroundEffect),
    };
  } catch {
    return defaultDevicePrefs();
  }
}

export function serializeDevicePrefsStorage(prefs: DevicePrefs): string {
  return JSON.stringify(prefs);
}

function readInitialPrefs(): DevicePrefs {
  if (typeof window === "undefined") {
    return defaultDevicePrefs();
  }
  return parseDevicePrefsStorage(window.localStorage.getItem(STORAGE_KEY));
}

export const $devicePrefs = atom<DevicePrefs>(readInitialPrefs());

$devicePrefs.subscribe((prefs) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, serializeDevicePrefsStorage(prefs));
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

export function setAudioProcessingPrefs(audioProcessing: AudioProcessingPrefs): void {
  $devicePrefs.set({ ...$devicePrefs.get(), audioProcessing });
}

export function setAiNoiseModel(aiNoiseModel: NoiseModelId | null): void {
  $devicePrefs.set({ ...$devicePrefs.get(), aiNoiseModel });
}

export function setBackgroundEffectPref(backgroundEffect: BackgroundEffect): void {
  $devicePrefs.set({ ...$devicePrefs.get(), backgroundEffect });
}

type SpeakerOutputSelectionEnvironment = {
  document?: Pick<Document, "createElement">;
  AudioContext?: { prototype?: { setSinkId?: unknown } } | undefined;
};

type AudioContextSinkConstructor = NonNullable<
  SpeakerOutputSelectionEnvironment["AudioContext"]
>;

export function isSpeakerOutputSelectionSupported(
  env: SpeakerOutputSelectionEnvironment = {
    document: typeof document === "undefined" ? undefined : document,
    AudioContext:
      typeof AudioContext === "undefined"
        ? undefined
        : (AudioContext as unknown as AudioContextSinkConstructor),
  },
): boolean {
  if (!env.document) return false;
  const audio = env.document.createElement("audio") as HTMLAudioElement &
    Partial<{ setSinkId: (id: string) => Promise<void> }>;
  return (
    typeof audio.setSinkId === "function" &&
    typeof env.AudioContext?.prototype?.setSinkId === "function"
  );
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

export type CallDevicePreferenceKind = "mic" | "cam" | "speaker";

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

export type ResolvedCallDevicePreference = {
  activeDeviceId: string;
  preferenceId: string | null;
  captureDeviceId: string | undefined;
  missing: boolean;
};

function enumeratedDevicesForKind(
  devices: EnumeratedDevices,
  kind: CallDevicePreferenceKind,
): readonly EnumeratedDevice[] {
  if (kind === "mic") return devices.mics;
  if (kind === "cam") return devices.cams;
  return devices.speakers;
}

/**
 * The one NotFoundError shape every missing-device path emits, so
 * `recordMediaIssue`'s classifier sees a single canonical name.
 */
export function missingCallDeviceError(kind: "mic" | "cam"): Error {
  const error = new Error(`${kind} device is no longer available`);
  error.name = "NotFoundError";
  return error;
}

export function hasEnumeratedCallDeviceId(
  devices: EnumeratedDevices,
  kind: CallDevicePreferenceKind,
  deviceId: string,
): boolean {
  return enumeratedDevicesForKind(devices, kind).some((device) => device.deviceId === deviceId);
}

export async function resolveCallDevicePreference(
  kind: CallDevicePreferenceKind,
  deviceId: string | null,
): Promise<ResolvedCallDevicePreference> {
  if (deviceId === null) {
    return {
      activeDeviceId: "default",
      preferenceId: null,
      captureDeviceId: undefined,
      missing: false,
    };
  }
  const devices = await enumerateCallDevices();
  if (hasEnumeratedCallDeviceId(devices, kind, deviceId)) {
    return {
      activeDeviceId: deviceId,
      preferenceId: deviceId,
      captureDeviceId: deviceId,
      missing: false,
    };
  }
  return {
    activeDeviceId: "default",
    preferenceId: null,
    captureDeviceId: undefined,
    missing: true,
  };
}

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
