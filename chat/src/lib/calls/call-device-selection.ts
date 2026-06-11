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
    setMicDevice(deviceId);
    await engine.setMicDevice(activeDeviceId);
    return;
  }
  if (kind === "cam") {
    setCamDevice(deviceId);
    await engine.setCameraDevice(activeDeviceId);
    return;
  }
  setSpeakerDevice(deviceId);
  await engine.setSpeakerDevice(activeDeviceId);
}
