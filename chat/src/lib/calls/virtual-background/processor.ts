import type {
  BackgroundProcessorOptions,
  BackgroundProcessorWrapper,
  SwitchBackgroundProcessorOptions,
} from "@livekit/track-processors";

export type VirtualBackgroundEffect =
  | { kind: "off" }
  | { kind: "blur" }
  | { kind: "image"; imageUrl: string };

export type ActiveVirtualBackgroundEffect = Exclude<
  VirtualBackgroundEffect,
  { kind: "off" }
>;

export type VideoBackgroundProcessor = BackgroundProcessorWrapper;

export type SwitchableVideoBackgroundProcessor = {
  name: string;
  mode?: BackgroundProcessorWrapper["mode"];
  switchTo(options: SwitchBackgroundProcessorOptions): Promise<void>;
};

const PROCESSOR_PREFIX = "waddle:virtual-background:";
const TASKS_VISION_FILE_SET = "/mediapipe/tasks-vision/wasm";
const SELFIE_SEGMENTER_MODEL = "/mediapipe/models/selfie_segmenter_landscape.tflite";
const BLUR_RADIUS = 16;
const TARGET_FPS = 24;

export function virtualBackgroundProcessorName(
  effect: ActiveVirtualBackgroundEffect,
): string {
  return `${PROCESSOR_PREFIX}${effect.kind}`;
}

function virtualBackgroundEffectFromProcessorName(
  name: string | undefined,
): VirtualBackgroundEffect {
  if (name === virtualBackgroundProcessorName({ kind: "blur" })) return { kind: "blur" };
  if (name === `${PROCESSOR_PREFIX}image`) return { kind: "image", imageUrl: "" };
  return { kind: "off" };
}

export function sameVirtualBackgroundEffect(
  a: VirtualBackgroundEffect,
  b: VirtualBackgroundEffect,
): boolean {
  if (a.kind !== b.kind) return false;
  return a.kind !== "image" || b.kind !== "image" || a.imageUrl === b.imageUrl;
}

export function liveKitBackgroundProcessorOptionsForEffect(
  effect: ActiveVirtualBackgroundEffect,
): BackgroundProcessorOptions {
  const common = {
    maxFps: TARGET_FPS,
    assetPaths: {
      tasksVisionFileSet: TASKS_VISION_FILE_SET,
      modelAssetPath: SELFIE_SEGMENTER_MODEL,
    },
  };
  if (effect.kind === "blur") {
    return {
      mode: "background-blur",
      blurRadius: BLUR_RADIUS,
      ...common,
    };
  }
  return {
    mode: "virtual-background",
    imagePath: effect.imageUrl,
    ...common,
  };
}

export function liveKitBackgroundProcessorSwitchOptionsForEffect(
  effect: VirtualBackgroundEffect,
): SwitchBackgroundProcessorOptions {
  if (effect.kind === "off") return { mode: "disabled" };
  if (effect.kind === "blur") return { mode: "background-blur", blurRadius: BLUR_RADIUS };
  return { mode: "virtual-background", imagePath: effect.imageUrl };
}

export function virtualBackgroundEffectFromProcessor(
  processor: { name?: string; mode?: string } | undefined,
  applied: VirtualBackgroundEffect,
): VirtualBackgroundEffect {
  if (!processor) return { kind: "off" };
  if (processor.mode === "disabled") return { kind: "off" };
  if (processor.mode === "background-blur") return { kind: "blur" };
  if (processor.mode === "virtual-background") {
    return applied.kind === "image" ? applied : { kind: "image", imageUrl: "" };
  }
  const nameEffect = virtualBackgroundEffectFromProcessorName(processor.name);
  if (nameEffect.kind === "off") return { kind: "off" };
  if (applied.kind !== "off") return applied;
  return nameEffect;
}

export function isSwitchableVirtualBackgroundProcessor(
  processor: { name?: string; switchTo?: unknown } | undefined,
): processor is SwitchableVideoBackgroundProcessor {
  if (!processor?.name?.startsWith(PROCESSOR_PREFIX)) return false;
  return typeof processor.switchTo === "function";
}
