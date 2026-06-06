<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Camera, Image as ImageIcon, Paperclip, RefreshCw, Square, Trash2, Video, X } from "lucide-vue-next";

type Mode = "attach" | "photo" | "record";
type MediaKind = "image" | "video";

interface StoryComposerProps {
  /** Disables submit + tab switching while the parent is uploading. */
  busy?: boolean;
  /** Hard size cap from XEP-0363 service. */
  maxBytes?: number;
  /** Removes the outer card chrome when nested inside a parent composer. */
  embedded?: boolean;
}

const props = withDefaults(defineProps<StoryComposerProps>(), {
  busy: false,
  maxBytes: 10 * 1024 * 1024,
  embedded: false,
});

const emit = defineEmits<{
  submit: [payload: { body?: string; file: Blob; mediaKind: MediaKind }];
  cancel: [];
}>();

// MediaRecorder MIME preference order: VP9 > VP8. iOS Safari rejects
// `video/mp4` as a `mimeType` argument and the only working path is to
// omit it entirely, letting Safari pick (it produces fragmented MP4).
const VIDEO_MIME_CANDIDATES = [
  "video/webm;codecs=vp9,opus",
  "video/webm;codecs=vp8,opus",
] as const;
const VIDEO_BITS_PER_SECOND = 2_000_000;
const RECORDING_MAX_MS = 20_000;
const RECORDING_TICK_MS = 100;

const mode = ref<Mode>("attach");
const body = ref("");
const errorMessage = ref<string | null>(null);

const previewUrl = ref<string | null>(null);
const previewBlob = ref<Blob | null>(null);
const previewKind = ref<MediaKind | null>(null);

const videoEl = ref<HTMLVideoElement | null>(null);
const stream = ref<MediaStream | null>(null);
const facingMode = ref<"user" | "environment">("user");
const cameraReady = ref(false);

const recorder = ref<MediaRecorder | null>(null);
const recording = ref(false);
const recordStartedAt = ref<number | null>(null);
const recordElapsedMs = ref(0);
let recordTimerId: ReturnType<typeof setInterval> | null = null;
let recordChunks: Blob[] = [];

const recordingTooBig = computed(
  () => previewBlob.value !== null && previewBlob.value.size > props.maxBytes,
);

const canSubmit = computed(() => {
  if (props.busy) return false;
  if (recordingTooBig.value) return false;
  if (recording.value) return false;
  if (previewBlob.value) return true;
  return false;
});

function setError(message: string | null) {
  errorMessage.value = message;
}

function setPreview(blob: Blob, kind: MediaKind) {
  revokePreviewUrl();
  previewBlob.value = blob;
  previewKind.value = kind;
  previewUrl.value = URL.createObjectURL(blob);
}

function revokePreviewUrl() {
  if (previewUrl.value !== null) {
    URL.revokeObjectURL(previewUrl.value);
    previewUrl.value = null;
  }
}

function clearPreview() {
  revokePreviewUrl();
  previewBlob.value = null;
  previewKind.value = null;
  setError(null);
}

function disposeCamera() {
  if (recorder.value) {
    // Detach handlers BEFORE stopping so the asynchronous `onstop`
    // event can't fire after we've cleared `recordChunks` and produce
    // a misleading zero-byte preview / spurious error toast. The
    // recorded buffer is discarded on dispose — by definition the
    // user has cancelled or switched away.
    recorder.value.ondataavailable = null;
    recorder.value.onstop = null;
    if (recording.value) {
      try {
        recorder.value.stop();
      } catch {
        /* ignore */
      }
    }
  }
  recorder.value = null;
  recording.value = false;
  recordStartedAt.value = null;
  recordElapsedMs.value = 0;
  recordChunks = [];
  if (recordTimerId !== null) {
    clearInterval(recordTimerId);
    recordTimerId = null;
  }
  if (stream.value) {
    for (const track of stream.value.getTracks()) {
      try {
        track.stop();
      } catch {
        /* ignore */
      }
    }
    stream.value = null;
  }
  cameraReady.value = false;
}

async function ensureCamera() {
  if (stream.value) return;
  if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
    setError("Camera capture isn't supported on this browser.");
    return;
  }
  setError(null);
  try {
    const next = await navigator.mediaDevices.getUserMedia({
      video: { facingMode: facingMode.value },
      audio: mode.value === "record",
    });
    stream.value = next;
    cameraReady.value = true;
    await attachStreamToVideo();
  } catch (err: unknown) {
    if (err && typeof err === "object" && "name" in err) {
      const name = (err as { name: string }).name;
      if (name === "NotAllowedError") {
        setError("Allow camera access in your browser to capture photos or video.");
      } else if (name === "NotFoundError") {
        setError("No camera detected.");
      } else {
        setError("Couldn't access the camera.");
      }
    } else {
      setError("Couldn't access the camera.");
    }
  }
}

async function attachStreamToVideo() {
  // Wait one tick so the `<video>` element exists when the user has
  // just switched into Photo or Record mode.
  await new Promise((resolve) => requestAnimationFrame(resolve));
  if (videoEl.value && stream.value) {
    videoEl.value.srcObject = stream.value;
    try {
      await videoEl.value.play();
    } catch {
      /* user gesture may be required — UI still shows the element */
    }
  }
}

async function flipCamera() {
  facingMode.value = facingMode.value === "user" ? "environment" : "user";
  disposeCamera();
  await ensureCamera();
}

async function activateMode(next: Mode) {
  if (props.busy) return;
  if (next === mode.value) return;
  // Switching away from a camera mode tears down the stream and any
  // in-flight recording — the user explicitly chose another mode.
  disposeCamera();
  mode.value = next;
  setError(null);
  if (next === "photo" || next === "record") {
    await ensureCamera();
  }
}

function onAttachFile(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  if (file.size > props.maxBytes) {
    setError(`That file is too large (max ${Math.floor(props.maxBytes / (1024 * 1024))} MB).`);
    return;
  }
  const kind: MediaKind = file.type.startsWith("video/") ? "video" : "image";
  setPreview(file, kind);
}

async function takePhoto() {
  if (!stream.value || !videoEl.value) {
    await ensureCamera();
  }
  const v = videoEl.value;
  if (!v) return;
  const w = v.videoWidth;
  const h = v.videoHeight;
  if (!w || !h) {
    setError("Camera isn't ready yet — try again in a moment.");
    return;
  }
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    setError("Couldn't capture the frame.");
    return;
  }
  ctx.drawImage(v, 0, 0, w, h);
  const blob: Blob | null = await new Promise((resolve) => {
    canvas.toBlob((b) => resolve(b), "image/jpeg", 0.92);
  });
  if (!blob) {
    setError("Couldn't capture the frame.");
    return;
  }
  if (blob.size > props.maxBytes) {
    setError(`That photo is too large (max ${Math.floor(props.maxBytes / (1024 * 1024))} MB). Try a lower-res camera.`);
    return;
  }
  setPreview(blob, "image");
}

function pickSupportedMimeType(): string | undefined {
  if (typeof MediaRecorder === "undefined") return undefined;
  for (const mime of VIDEO_MIME_CANDIDATES) {
    if (MediaRecorder.isTypeSupported(mime)) return mime;
  }
  return undefined;
}

function startRecording() {
  if (!stream.value) {
    setError("Camera not ready.");
    return;
  }
  if (typeof MediaRecorder === "undefined") {
    setError("Recording isn't supported on this browser.");
    return;
  }
  const mimeType = pickSupportedMimeType();
  let mr: MediaRecorder;
  try {
    mr = mimeType
      ? new MediaRecorder(stream.value, { mimeType, videoBitsPerSecond: VIDEO_BITS_PER_SECOND })
      : new MediaRecorder(stream.value, { videoBitsPerSecond: VIDEO_BITS_PER_SECOND });
  } catch {
    setError("Recording isn't supported on this browser.");
    return;
  }
  recordChunks = [];
  mr.ondataavailable = (event) => {
    if (event.data && event.data.size > 0) recordChunks.push(event.data);
  };
  mr.onstop = () => {
    const blob = new Blob(recordChunks, { type: mr.mimeType || "video/webm" });
    recordChunks = [];
    recording.value = false;
    recordStartedAt.value = null;
    recordElapsedMs.value = 0;
    if (recordTimerId !== null) {
      clearInterval(recordTimerId);
      recordTimerId = null;
    }
    if (blob.size === 0) {
      setError("Recording produced no data.");
      return;
    }
    if (blob.size > props.maxBytes) {
      setError(`That recording is too large (over ${Math.floor(props.maxBytes / (1024 * 1024))} MB). Try a shorter clip.`);
      // Still show preview so the user can see what happened? No —
      // they can't upload, so just discard.
      return;
    }
    setPreview(blob, "video");
  };
  setError(null);
  recorder.value = mr;
  mr.start();
  recording.value = true;
  recordStartedAt.value = performance.now();
  recordTimerId = setInterval(() => {
    if (recordStartedAt.value === null) return;
    recordElapsedMs.value = performance.now() - recordStartedAt.value;
    if (recordElapsedMs.value >= RECORDING_MAX_MS) {
      stopRecording();
    }
  }, RECORDING_TICK_MS);
}

function stopRecording() {
  const mr = recorder.value;
  if (!mr) return;
  if (mr.state !== "inactive") {
    try {
      mr.stop();
    } catch {
      /* swallow */
    }
  }
}

function submit() {
  if (!previewBlob.value || !previewKind.value) return;
  emit("submit", {
    ...(body.value.trim() ? { body: body.value.trim() } : {}),
    file: previewBlob.value,
    mediaKind: previewKind.value,
  });
}

function cancel() {
  disposeCamera();
  clearPreview();
  body.value = "";
  emit("cancel");
}

watch(() => props.busy, (busy) => {
  // When the parent starts uploading, freeze the camera. We don't
  // tear down so the user can re-shoot if upload fails.
  if (busy && recording.value) stopRecording();
});

onBeforeUnmount(() => {
  disposeCamera();
  revokePreviewUrl();
});
</script>

<template>
  <section :class="embedded ? 'grid gap-3' : 'grid gap-3 rounded-lg border border-border bg-card p-3'">
    <div role="tablist" aria-label="Composer mode" class="flex gap-1 rounded-md bg-muted/40 p-1">
      <button
        type="button"
        role="tab"
        :aria-selected="mode === 'attach'"
        class="flex-1 inline-flex items-center justify-center gap-1.5 rounded px-2.5 py-1.5 text-xs font-medium"
        :class="mode === 'attach' ? 'bg-background text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
        :disabled="busy"
        @click="activateMode('attach')"
      >
        <Paperclip class="h-3.5 w-3.5" aria-hidden="true" />
        Attach
      </button>
      <button
        type="button"
        role="tab"
        :aria-selected="mode === 'photo'"
        class="flex-1 inline-flex items-center justify-center gap-1.5 rounded px-2.5 py-1.5 text-xs font-medium"
        :class="mode === 'photo' ? 'bg-background text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
        :disabled="busy"
        @click="activateMode('photo')"
      >
        <ImageIcon class="h-3.5 w-3.5" aria-hidden="true" />
        Photo
      </button>
      <button
        type="button"
        role="tab"
        :aria-selected="mode === 'record'"
        class="flex-1 inline-flex items-center justify-center gap-1.5 rounded px-2.5 py-1.5 text-xs font-medium"
        :class="mode === 'record' ? 'bg-background text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
        :disabled="busy"
        @click="activateMode('record')"
      >
        <Video class="h-3.5 w-3.5" aria-hidden="true" />
        Record
      </button>
    </div>

    <p v-if="errorMessage" class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
      {{ errorMessage }}
    </p>

    <div v-if="mode === 'attach' && !previewUrl" class="grid place-items-center rounded-md border border-dashed border-border px-3 py-6 text-center">
      <label class="inline-flex cursor-pointer items-center gap-2 rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm hover:bg-muted/50">
        <Paperclip class="h-4 w-4" aria-hidden="true" />
        Choose photo or video
        <input
          type="file"
          accept="image/*,video/*"
          class="sr-only"
          :disabled="busy"
          @change="onAttachFile"
        />
      </label>
      <p class="mt-2 type-caption text-muted-foreground">
        Max {{ Math.floor(maxBytes / (1024 * 1024)) }} MB
      </p>
    </div>

    <div v-if="(mode === 'photo' || mode === 'record') && !previewUrl" class="grid gap-2">
      <div class="relative overflow-hidden rounded-md bg-black">
        <video
          ref="videoEl"
          class="block max-h-[60vh] w-full"
          playsinline
          muted
          autoplay
        ></video>
        <button
          type="button"
          class="absolute top-2 right-2 inline-flex h-8 w-8 items-center justify-center rounded-full bg-background/80 text-foreground shadow-sm hover:bg-background"
          :disabled="busy || recording"
          :aria-label="facingMode === 'user' ? 'Switch to back camera' : 'Switch to front camera'"
          @click="flipCamera"
        >
          <RefreshCw class="h-4 w-4" aria-hidden="true" />
        </button>
        <div v-if="recording" class="absolute top-2 left-2 inline-flex items-center gap-1.5 rounded-full bg-destructive/90 px-2.5 py-0.5 text-xs font-medium text-destructive-foreground">
          <span class="h-2 w-2 rounded-full bg-destructive-foreground animate-pulse"></span>
          {{ Math.ceil((RECORDING_MAX_MS - recordElapsedMs) / 1000) }}s
        </div>
      </div>
      <div class="flex items-center justify-center gap-2">
        <button
          v-if="mode === 'photo'"
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="busy || !cameraReady"
          @click="takePhoto"
        >
          <Camera class="h-4 w-4" aria-hidden="true" />
          Capture
        </button>
        <template v-else>
          <button
            v-if="!recording"
            type="button"
            class="inline-flex items-center gap-1.5 rounded-md bg-destructive px-3 py-2 text-sm font-medium text-destructive-foreground shadow-sm hover:bg-destructive/90 disabled:cursor-not-allowed disabled:opacity-60"
            :disabled="busy || !cameraReady"
            @click="startRecording"
          >
            <Video class="h-4 w-4" aria-hidden="true" />
            Record
          </button>
          <button
            v-else
            type="button"
            class="inline-flex items-center gap-1.5 rounded-md bg-foreground px-3 py-2 text-sm font-medium text-background shadow-sm hover:opacity-90"
            @click="stopRecording"
          >
            <Square class="h-4 w-4" aria-hidden="true" />
            Stop
          </button>
        </template>
      </div>
    </div>

    <div v-if="previewUrl" class="grid gap-2">
      <div class="relative overflow-hidden rounded-md bg-black">
        <img
          v-if="previewKind === 'image'"
          :src="previewUrl"
          alt="Story preview"
          class="block max-h-[60vh] w-full object-contain"
        />
        <video
          v-else
          :src="previewUrl"
          class="block max-h-[60vh] w-full"
          controls
          playsinline
        ></video>
        <button
          type="button"
          class="absolute top-2 right-2 inline-flex h-8 w-8 items-center justify-center rounded-full bg-background/80 text-foreground shadow-sm hover:bg-background"
          :disabled="busy"
          aria-label="Remove media"
          @click="clearPreview"
        >
          <Trash2 class="h-4 w-4" aria-hidden="true" />
        </button>
      </div>
    </div>

    <textarea
      v-model="body"
      class="min-h-[3rem] w-full resize-y rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      placeholder="Add a caption (optional)"
      :disabled="busy"
      aria-label="Story caption"
    />

    <div class="flex items-center justify-between gap-2">
      <span class="type-caption text-muted-foreground">Expires in 24h</span>
      <div class="flex items-center gap-2">
        <button
          type="button"
          class="inline-flex items-center gap-1 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
          :disabled="busy"
          @click="cancel"
        >
          <X class="h-3.5 w-3.5" aria-hidden="true" />
          Cancel
        </button>
        <button
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="!canSubmit"
          @click="submit"
        >
          {{ busy ? "Sharing…" : "Share" }}
        </button>
      </div>
    </div>
  </section>
</template>
