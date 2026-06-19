import { Track, type TrackProcessor, type VideoProcessorOptions } from "livekit-client";
import type { ImageSegmenter, ImageSegmenterResult } from "@mediapipe/tasks-vision";
import { isAllowedVirtualBackgroundImageUrl } from "../device-prefs";

export type VirtualBackgroundEffect =
  | { kind: "off" }
  | { kind: "blur" }
  | { kind: "image"; imageUrl: string };

export type ActiveVirtualBackgroundEffect = Exclude<
  VirtualBackgroundEffect,
  { kind: "off" }
>;

export type VideoBackgroundProcessor = TrackProcessor<Track.Kind.Video, VideoProcessorOptions>;

const PROCESSOR_PREFIX = "waddle:virtual-background:";

function virtualBackgroundProcessorName(
  effect: ActiveVirtualBackgroundEffect,
): string {
  return `${PROCESSOR_PREFIX}${effect.kind}`;
}

export function virtualBackgroundEffectFromProcessorName(
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

const MEDIAPIPE_WASM_ROOT = "/mediapipe/wasm";
const SELFIE_SEGMENTER_MODEL = "/mediapipe/selfie_segmenter_landscape.tflite";
const TARGET_FPS = 24;
const FOREGROUND_ALPHA_START = 0.35;
const FOREGROUND_ALPHA_END = 0.75;
const FOREGROUND_TEMPORAL_SMOOTHING = 0.6;
const MASK_FEATHER = "blur(3px)";

let segmenterPromise: Promise<ImageSegmenter> | undefined;
let segmenterUsers = 0;

async function loadSegmenter(): Promise<ImageSegmenter> {
  if (!segmenterPromise) {
    segmenterPromise = import("@mediapipe/tasks-vision")
      .then(async ({ FilesetResolver, ImageSegmenter }) => {
        const vision = await FilesetResolver.forVisionTasks(MEDIAPIPE_WASM_ROOT);
        return ImageSegmenter.createFromOptions(vision, {
          baseOptions: {
            modelAssetPath: SELFIE_SEGMENTER_MODEL,
            delegate: "CPU",
          },
          runningMode: "VIDEO",
          outputCategoryMask: false,
          outputConfidenceMasks: true,
        });
      })
      .catch((error) => {
        segmenterPromise = undefined;
        throw error;
      });
  }
  return segmenterPromise;
}

function retainSegmenter(): void {
  segmenterUsers += 1;
}

async function releaseSegmenter(): Promise<void> {
  segmenterUsers = Math.max(0, segmenterUsers - 1);
  if (segmenterUsers !== 0 || !segmenterPromise) return;
  const segmenter = await segmenterPromise.catch(() => undefined);
  segmenter?.close();
  segmenterPromise = undefined;
}

type VirtualBackgroundGraph = {
  source: HTMLVideoElement;
  canvas: HTMLCanvasElement;
  context: CanvasRenderingContext2D;
  stream: MediaStream;
  inputStream: MediaStream;
  frameTimer?: ReturnType<typeof setInterval>;
  image?: HTMLImageElement;
  rendering: boolean;
  maskCanvas: HTMLCanvasElement;
  maskContext: CanvasRenderingContext2D;
  maskImageData: ImageData;
  maskAlpha: Uint8ClampedArray;
  maskHasHistory: boolean;
  retainedSegmenter: boolean;
};

type FrameTimer = ReturnType<typeof setInterval>;

type VirtualBackgroundProcessorDeps = {
  loadSegmenter?: () => Promise<ImageSegmenter>;
  setFrameTimer?: (handler: TimerHandler, timeout?: number) => FrameTimer;
  clearFrameTimer?: (timer: FrameTimer) => void;
};

export class CanvasVirtualBackgroundProcessor implements VideoBackgroundProcessor {
  readonly name: string;
  processedTrack?: MediaStreamTrack;

  private graph?: VirtualBackgroundGraph;
  private readonly loadSegmenter: () => Promise<ImageSegmenter>;
  private readonly setFrameTimer: (handler: TimerHandler, timeout?: number) => FrameTimer;
  private readonly clearFrameTimer: (timer: FrameTimer) => void;

  constructor(
    private readonly effect: ActiveVirtualBackgroundEffect,
    deps: VirtualBackgroundProcessorDeps = {},
  ) {
    this.name = virtualBackgroundProcessorName(effect);
    this.loadSegmenter = deps.loadSegmenter ?? loadSegmenter;
    this.setFrameTimer = deps.setFrameTimer ??
      ((handler, timeout) => globalThis.setInterval(handler, timeout) as unknown as FrameTimer);
    this.clearFrameTimer = deps.clearFrameTimer ??
      ((timer) => globalThis.clearInterval(timer));
  }

  async init(opts: VideoProcessorOptions): Promise<void> {
    const graph = await this.buildGraph(opts);
    this.graph = graph;
    this.processedTrack = graph.stream.getVideoTracks()[0];
  }

  async restart(opts: VideoProcessorOptions): Promise<void> {
    const next = await this.buildGraph(opts);
    const previous = this.graph;
    this.graph = next;
    this.processedTrack = next.stream.getVideoTracks()[0];
    if (previous) this.teardownGraph(previous);
  }

  async destroy(): Promise<void> {
    if (this.graph) this.teardownGraph(this.graph);
    this.graph = undefined;
    this.processedTrack = undefined;
  }

  private async buildGraph(opts: VideoProcessorOptions): Promise<VirtualBackgroundGraph> {
    const source = document.createElement("video");
    source.muted = true;
    source.playsInline = true;
    source.autoplay = true;
    const inputStream = new MediaStream([opts.track]);
    source.srcObject = inputStream;
    await source.play();
    await waitForVideoDimensions(source);

    const canvas = document.createElement("canvas");
    canvas.width = source.videoWidth || opts.track.getSettings().width || 1280;
    canvas.height = source.videoHeight || opts.track.getSettings().height || 720;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("Canvas 2D context is unavailable");
    const maskCanvas = document.createElement("canvas");
    maskCanvas.width = 1;
    maskCanvas.height = 1;
    const maskContext = maskCanvas.getContext("2d");
    if (!maskContext) throw new Error("Mask canvas 2D context is unavailable");
    const stream = canvas.captureStream(TARGET_FPS);
    const graph: VirtualBackgroundGraph = {
      source,
      canvas,
      context,
      stream,
      inputStream,
      rendering: false,
      maskCanvas,
      maskContext,
      maskImageData: new ImageData(1, 1),
      maskAlpha: new Uint8ClampedArray(1),
      maskHasHistory: false,
      retainedSegmenter: false,
    };
    try {
      graph.image = this.effect.kind === "image"
        ? await loadBackgroundImage(this.effect.imageUrl)
        : undefined;
      await this.renderFrame(graph);
      graph.frameTimer = this.setFrameTimer(() => {
        void this.renderFrame(graph);
      }, Math.round(1000 / TARGET_FPS));
      return graph;
    } catch (error) {
      this.teardownGraph(graph);
      throw error;
    }
  }

  private async renderFrame(graph: VirtualBackgroundGraph): Promise<void> {
    if (graph.rendering) return;
    if (graph.source.readyState < HTMLMediaElement.HAVE_CURRENT_DATA) return;
    graph.rendering = true;
    try {
      if (!graph.retainedSegmenter) {
        retainSegmenter();
        graph.retainedSegmenter = true;
      }
      const segmenter = await this.loadSegmenter();
      const result = segmenter.segmentForVideo(graph.source, performance.now());
      drawVirtualBackground(graph, this.effect, result);
    } finally {
      graph.rendering = false;
    }
  }

  private teardownGraph(graph: VirtualBackgroundGraph): void {
    if (graph.frameTimer !== undefined) {
      this.clearFrameTimer(graph.frameTimer);
      graph.frameTimer = undefined;
    }
    graph.source.pause();
    graph.source.srcObject = null;
    for (const track of graph.stream.getTracks()) track.stop();
    if (graph.retainedSegmenter) {
      graph.retainedSegmenter = false;
      void releaseSegmenter();
    }
  }
}

function waitForVideoDimensions(video: HTMLVideoElement): Promise<void> {
  if (video.videoWidth > 0 && video.videoHeight > 0) return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      video.removeEventListener("loadedmetadata", done);
      resolve();
    };
    video.addEventListener("loadedmetadata", done, { once: true });
  });
}

function loadBackgroundImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    if (!isAllowedVirtualBackgroundImageUrl(url)) {
      reject(new Error("Unsupported virtual background image URL"));
      return;
    }
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("Could not load virtual background image"));
    image.src = url;
  });
}

function drawVirtualBackground(
  graph: VirtualBackgroundGraph,
  effect: ActiveVirtualBackgroundEffect,
  result: ImageSegmenterResult,
): void {
  const mask = result.confidenceMasks?.[0];
  if (!mask) {
    graph.context.drawImage(graph.source, 0, 0, graph.canvas.width, graph.canvas.height);
    result.close();
    return;
  }
  const { context, canvas, source } = graph;
  context.save();
  context.clearRect(0, 0, canvas.width, canvas.height);

  if (effect.kind === "image" && graph.image) {
    drawCover(context, graph.image, canvas.width, canvas.height);
  } else {
    context.filter = "blur(16px)";
    context.drawImage(source, 0, 0, canvas.width, canvas.height);
    context.filter = "none";
  }

  context.globalCompositeOperation = "destination-out";
  context.filter = MASK_FEATHER;
  context.drawImage(
    updateMaskCanvas(graph, mask.getAsFloat32Array(), mask.width, mask.height),
    0,
    0,
    canvas.width,
    canvas.height,
  );
  context.filter = "none";
  context.globalCompositeOperation = "destination-over";
  context.drawImage(source, 0, 0, canvas.width, canvas.height);
  context.restore();
  result.close();
}

function updateMaskCanvas(
  graph: VirtualBackgroundGraph,
  mask: Float32Array,
  width: number,
  height: number,
): HTMLCanvasElement {
  if (graph.maskCanvas.width !== width || graph.maskCanvas.height !== height) {
    graph.maskCanvas.width = width;
    graph.maskCanvas.height = height;
    graph.maskImageData = new ImageData(width, height);
    graph.maskAlpha = new Uint8ClampedArray(mask.length);
    graph.maskHasHistory = false;
  }
  const imageData = graph.maskImageData;
  const alpha = graph.maskAlpha;
  writeVirtualBackgroundAlphaFromConfidence(mask, alpha, graph.maskHasHistory);
  graph.maskHasHistory = true;
  for (let i = 0; i < mask.length; i += 1) {
    const pixel = i * 4;
    imageData.data[pixel] = 0;
    imageData.data[pixel + 1] = 0;
    imageData.data[pixel + 2] = 0;
    imageData.data[pixel + 3] = alpha[i] ?? 0;
  }
  graph.maskContext.putImageData(imageData, 0, 0);
  return graph.maskCanvas;
}

export function virtualBackgroundAlphaFromConfidence(
  confidence: Float32Array | readonly number[],
  previousAlpha?: Uint8ClampedArray,
): Uint8ClampedArray {
  const alpha = previousAlpha
    ? Uint8ClampedArray.from(previousAlpha)
    : new Uint8ClampedArray(confidence.length);
  writeVirtualBackgroundAlphaFromConfidence(confidence, alpha, Boolean(previousAlpha));
  return alpha;
}

function writeVirtualBackgroundAlphaFromConfidence(
  confidence: Float32Array | readonly number[],
  alpha: Uint8ClampedArray,
  smoothAgainstPrevious: boolean,
): void {
  for (let i = 0; i < confidence.length; i += 1) {
    const nextAlpha = confidenceToForegroundAlpha(confidence[i] ?? 0);
    alpha[i] = smoothAgainstPrevious
      ? Math.round(
        (alpha[i] ?? 0) * FOREGROUND_TEMPORAL_SMOOTHING +
          nextAlpha * (1 - FOREGROUND_TEMPORAL_SMOOTHING),
      )
      : nextAlpha;
  }
}

function confidenceToForegroundAlpha(confidence: number): number {
  const t = Math.min(
    1,
    Math.max(0, (confidence - FOREGROUND_ALPHA_START) / (FOREGROUND_ALPHA_END - FOREGROUND_ALPHA_START)),
  );
  const smoothed = t * t * (3 - 2 * t);
  return Math.round(smoothed * 255);
}

function drawCover(
  context: CanvasRenderingContext2D,
  image: CanvasImageSource & { width: number; height: number },
  width: number,
  height: number,
): void {
  const scale = Math.max(width / image.width, height / image.height);
  const drawWidth = image.width * scale;
  const drawHeight = image.height * scale;
  context.drawImage(
    image,
    (width - drawWidth) / 2,
    (height - drawHeight) / 2,
    drawWidth,
    drawHeight,
  );
}
