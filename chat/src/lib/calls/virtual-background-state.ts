import { atom } from "nanostores";
import {
  sameVirtualBackgroundEffect,
  type VirtualBackgroundEffect,
} from "./virtual-background/processor";

export const $virtualBackground = atom<VirtualBackgroundEffect>({ kind: "off" });

export function setVirtualBackground(state: VirtualBackgroundEffect): void {
  if (sameVirtualBackgroundEffect($virtualBackground.get(), state)) return;
  $virtualBackground.set(state);
}

export function resetVirtualBackground(): void {
  setVirtualBackground({ kind: "off" });
}
