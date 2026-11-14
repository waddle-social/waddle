<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Clock, Image as ImageIcon, Paperclip, RefreshCw, Trash2, Upload, Video } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, floe, input, seg } from "styled-system/recipes";

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

const ui = {
  root: css({ display: "flex", flexDirection: "column", gap: "3" }),
  card: cx(floe(), css({ "--pad": "16px", gap: "3" })),
  modes: cx(seg(), css({ alignSelf: "flex-start" })),
  alert: css({ margin: 0, padding: "10px 12px", borderRadius: "10px", bg: "danger.subtle", border: "1px solid {colors.danger}", color: "danger.text", fontSize: "13px" }),
  drop: css({
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    gap: "2",
    minHeight: "180px",
    borderRadius: "control",
    border: "2px dashed {colors.border}",
    "&:has(input:focus-visible) label": { outline: "{borderWidths.focus} solid {colors.focus}", outlineOffset: "2px" },
  }),
  srOnly: css({ srOnly: true }),
  caption: css({ textStyle: "caption", color: "fg.muted" }),
  stage: css({
    position: "relative",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    height: "300px",
    maxHeight: "60vh",
    borderRadius: "control",
    overflow: "hidden",
    bgImage: "{gradients.sea}",
    "& > video": { display: "block", width: "100%", height: "100%", objectFit: "cover" },
    "&[data-blocked]": { opacity: 0.35 },
  }),
  preview: css({ bg: "sea.shallow", bgImage: "none", "& > img": { display: "block", height: "90%", maxWidth: "92%", objectFit: "contain" }, "& > video[controls]": { objectFit: "contain" } }),
  round: css({
    position: "absolute",
    top: "10px",
    right: "10px",
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    width: "44px",
    height: "44px",
    border: 0,
    borderRadius: "pill",
    bg: "material.media",
    color: "fg",
    cursor: "pointer",
    "& svg": { width: "20px", height: "20px" },
    _hover: { bgImage: "linear-gradient({colors.state.hover}, {colors.state.hover})" },
    _disabled: { opacity: 0.5, cursor: "not-allowed" },
  }),
  timer: css({
    position: "absolute",
    top: "12px",
    left: "12px",
    display: "inline-flex",
    alignItems: "center",
    gap: "1.5",
    height: "28px",
    px: "2.5",
    borderRadius: "pill",
    bg: "live",
    color: "live.fg",
    font: "700 13px/1 {fonts.mono}",
    "& i": { width: "8px", height: "8px", borderRadius: "pill", bg: "live.fg", animation: "wd-pulse 1s infinite" },
  }),
  shutter: css({ position: "absolute", left: 0, right: 0, bottom: "14px", display: "flex", justifyContent: "center", "& .wd-btn": { height: "44px", borderRadius: "pill", px: "22px" } }),
  textarea: cx(input(), css({ height: "auto", minHeight: "3rem", py: "2.5", resize: "none", font: "14px/1.5 {fonts.body}" })),
  foot: css({ display: "flex", alignItems: "center", gap: "2" }),
  expires: css({ flex: 1, display: "inline-flex", alignItems: "center", gap: "1.5", textStyle: "caption", color: "fg.muted", "& svg": { width: "14px", height: "14px" } }),
};
const quietButton = btn();
const primaryButton = btn({ variant: "primary" });
const liveButton = btn({ variant: "live" });
const dangerButton = btn({ variant: "danger" });

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
  <section :class="embedded ? ui.root : ui.card">
    <div role="tablist" aria-label="Composer mode" :class="ui.modes">
      <button
        type="button"
        role="tab"
        :aria-selected="mode === 'attach'"
        :disabled="busy"
        @click="activateMode('attach')"
      >
        <Paperclip aria-hidden="true" />
        Attach
      </button>
      <button
        type="button"
        role="tab"
        :aria-selected="mode === 'photo'"
        :disabled="busy"
        @click="activateMode('photo')"
      >
        <ImageIcon aria-hidden="true" />
        Photo
      </button>
      <button
        type="button"
        role="tab"
        :aria-selected="mode === 'record'"
        :disabled="busy"
        @click="activateMode('record')"
      >
        <Video aria-hidden="true" />
        Record
      </button>
    </div>

    <p v-if="errorMessage" :class="ui.alert">
      {{ errorMessage }}
    </p>

    <div v-if="mode === 'attach' && !previewUrl" :class="ui.drop">
      <label :class="quietButton">
        <Upload aria-hidden="true" />
        Choose photo or video
        <input
          type="file"
          accept="image/*,video/*"
          :class="ui.srOnly"
          :disabled="busy"
          @change="onAttachFile"
        />
      </label>
      <span :class="ui.caption">
        Max {{ Math.floor(maxBytes / (1024 * 1024)) }} MB
      </span>
    </div>

    <div
      v-if="(mode === 'photo' || mode === 'record') && !previewUrl"
      :class="ui.stage"
      :data-blocked="errorMessage && !cameraReady ? '' : undefined"
      data-theme="night"
    >
      <video
        ref="videoEl"
        playsinline
        muted
        autoplay
      ></video>
      <button
        type="button"
        :class="ui.round"
        :disabled="busy || recording"
        :aria-label="facingMode === 'user' ? 'Switch to back camera' : 'Switch to front camera'"
        @click="flipCamera"
      >
        <RefreshCw aria-hidden="true" />
      </button>
      <div v-if="recording" :class="ui.timer">
        <i aria-hidden="true"></i>
        {{ Math.ceil((RECORDING_MAX_MS - recordElapsedMs) / 1000) }}s
      </div>
      <div :class="ui.shutter">
        <button
          v-if="mode === 'photo'"
          type="button"
          :class="primaryButton"
          :disabled="busy || !cameraReady"
          @click="takePhoto"
        >
          Capture
        </button>
        <template v-else>
          <button
            v-if="!recording"
            type="button"
            :class="liveButton"
            :disabled="busy || !cameraReady"
            @click="startRecording"
          >
            Record
          </button>
          <button
            v-else
            type="button"
            :class="dangerButton"
            @click="stopRecording"
          >
            Stop
          </button>
        </template>
      </div>
    </div>

    <div v-if="previewUrl" :class="[ui.stage, ui.preview]">
      <img
        v-if="previewKind === 'image'"
        :src="previewUrl"
        alt="Story preview"
      />
      <video
        v-else
        :src="previewUrl"
        controls
        playsinline
      ></video>
      <button
        type="button"
        :class="ui.round"
        data-theme="night"
        :disabled="busy"
        aria-label="Remove media"
        @click="clearPreview"
      >
        <Trash2 aria-hidden="true" />
      </button>
    </div>

    <textarea
      v-model="body"
      :class="ui.textarea"
      rows="2"
      placeholder="Add a caption (optional)"
      :disabled="busy"
      aria-label="Story caption"
    />

    <div :class="ui.foot">
      <span :class="ui.expires"><Clock aria-hidden="true" />Expires in 24h</span>
      <button
        type="button"
        :class="quietButton"
        :disabled="busy"
        @click="cancel"
      >
        Cancel
      </button>
      <button
        type="button"
        :class="primaryButton"
        :disabled="!canSubmit"
        @click="submit"
      >
        {{ busy ? "Sharing…" : "Share" }}
      </button>
    </div>
  </section>
</template>
