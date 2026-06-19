import type {
  ActiveVirtualBackgroundEffect,
  VideoBackgroundProcessor,
} from "./processor";
import { CanvasVirtualBackgroundProcessor } from "./processor";

export async function makeVirtualBackgroundProcessor(
  effect: ActiveVirtualBackgroundEffect,
): Promise<VideoBackgroundProcessor> {
  return new CanvasVirtualBackgroundProcessor(effect);
}
