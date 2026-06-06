import { atom } from "nanostores";
import { clearMediaIssue } from "./call-media-issues";

export const $callScreenShareEnabled = atom<boolean>(false);
export const $callScreenShareSupported = atom<boolean>(canUseDisplayMedia());

function canUseDisplayMedia(): boolean {
  return typeof navigator !== "undefined" &&
    typeof navigator.mediaDevices?.getDisplayMedia === "function";
}

export function syncScreenShareEnabled(enabled: boolean): void {
  $callScreenShareEnabled.set(enabled);
  if (!enabled) clearMediaIssue("screen");
}
