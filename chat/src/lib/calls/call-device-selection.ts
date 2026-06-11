import {
  setCamDevice,
  setMicDevice,
  setSpeakerDevice,
} from "./device-prefs";

export type CallDeviceKind = "mic" | "cam" | "speaker";

export type CallDeviceSelectionEngine = {
  setMicDevice(deviceId: string): Promise<void>;
  setCameraDevice(deviceId: string): Promise<void>;
  setSpeakerDevice(deviceId: string): Promise<void>;
};

export async function applyCallDeviceSelection(
  kind: CallDeviceKind,
  deviceId: string | null,
  engine: CallDeviceSelectionEngine,
): Promise<void> {
  const activeDeviceId = deviceId ?? "default";
  if (kind === "mic") {
    await engine.setMicDevice(activeDeviceId);
    setMicDevice(deviceId);
    return;
  }
  if (kind === "cam") {
    await engine.setCameraDevice(activeDeviceId);
    setCamDevice(deviceId);
    return;
  }
  await engine.setSpeakerDevice(activeDeviceId);
  setSpeakerDevice(deviceId);
}
