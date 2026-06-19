import { BackgroundProcessor, supportsBackgroundProcessors } from "@livekit/track-processors";
import type {
  ActiveVirtualBackgroundEffect,
  VideoBackgroundProcessor,
} from "./processor";
import {
  liveKitBackgroundProcessorOptionsForEffect,
  virtualBackgroundProcessorName,
} from "./processor";

type BackgroundProcessorSupportProbe = () => boolean;

export function isVirtualBackgroundProcessorSupported(
  supportProbe: BackgroundProcessorSupportProbe = supportsBackgroundProcessors,
): boolean {
  try {
    return supportProbe();
  } catch {
    return false;
  }
}

export async function makeVirtualBackgroundProcessor(
  effect: ActiveVirtualBackgroundEffect,
  supportProbe?: BackgroundProcessorSupportProbe,
): Promise<VideoBackgroundProcessor> {
  if (!isVirtualBackgroundProcessorSupported(supportProbe)) {
    throw new Error("Virtual background processors are not supported in this browser");
  }
  return BackgroundProcessor(
    liveKitBackgroundProcessorOptionsForEffect(effect),
    virtualBackgroundProcessorName(effect),
  );
}
