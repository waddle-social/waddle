<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Activity, Check, ImagePlus, Mic, Speaker, Video, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import { css, cx } from "styled-system/css";
import { btn, iconbtn, kicker } from "styled-system/recipes";
import { dialogParts } from "@/ui/part-classes";
import {
  $devicePrefs,
  enumerateCallDevices,
  isSpeakerOutputSelectionSupported,
  type EnumeratedDevices,
  type AudioProcessingPrefs,
} from "@/lib/calls/device-prefs";
import {
  applyAiNoiseModelSelection,
  applyAudioProcessingSelection,
  applyBackgroundEffectSelection,
  applyCallDeviceSelection,
} from "@/lib/calls/call-device-selection";
import { $micAudioProcessing } from "@/lib/calls/mic-audio-processing-state";
import { audioProcessingRows } from "@/lib/calls/mic-audio-processing";
import { $micAiNoiseFilter } from "@/lib/calls/mic-ai-noise-filter-state";
import { aiNoiseFilterRow } from "@/lib/calls/ai-noise-filter/mic-ai-noise-filter";
import {
  noiseModelMeta,
  orderedNoiseModelMetas,
} from "@/lib/calls/ai-noise-filter/model-metadata";
import type { NoiseModelId } from "@/lib/calls/ai-noise-filter/model-id";
import {
  anyNoiseModelAvailable,
  currentNoiseModelSupportEnv,
  noiseModelSupport,
} from "@/lib/calls/ai-noise-filter/support";
import { $aiNoiseFilterError } from "@/lib/calls/ai-noise-filter-error-state";
import { $cameraBackground } from "@/lib/calls/camera-background-effect-state";
import { $backgroundEffectError } from "@/lib/calls/background-effect-error-state";
import {
  backgroundEffectSupport,
  currentBackgroundEffectSupportEnv,
} from "@/lib/calls/background-effect/support";
import { backgroundCatalog } from "@/lib/calls/background-effect/backgrounds";
import { saveCustomBackground } from "@/lib/calls/background-effect/custom-image-store";
import {
  BACKGROUND_OFF,
  sameBackgroundEffect,
  type BackgroundEffect,
  type BackgroundImageId,
} from "@/lib/calls/background-effect/effect-id";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { reportCallError } from "@/lib/calls/call-store";
import {
  describeVideoStats,
  summarizeVideoStats,
  type CallStatRow,
  type CallStatSample,
} from "@/lib/calls/call-stats";

/**
 * Settings dialog the call surface opens for mic/camera/output
 * selection.
 *
 * Lives in a `<AppDialog>` (matches the rest of the chat app's
 * modal style: same backdrop blur, same close-on-escape behavior,
 * same focus trap via aria-modal). The picker rows enumerate the
 * browser's `MediaDeviceInfo` list and persist the user's choice
 * through `$devicePrefs`; switching device while a call is active
 * is applied via the LiveKit engine without disconnecting.
 *
 * Speaker selection requires sink routing for both the media element
 * and the Web Audio context. Browsers without that full path keep the
 * picker disabled rather than surfacing a broken output switch.
 */
const open = defineModel<boolean>("open", { required: true });

const prefs = useStore($devicePrefs);
const devices = ref<EnumeratedDevices>({ mics: [], cams: [], speakers: [] });
const speakerSupported = ref(isSpeakerOutputSelectionSupported());

/**
 * Verified (applied) browser-native audio processing of the local mic.
 * `no-mic` renders a single neutral line; an active mic renders the
 * tiered noise-cancellation / echo / auto-gain readout.
 */
const micProcessing = useStore($micAudioProcessing);
const processingRows = computed(() =>
  micProcessing.value.kind === "active" ? audioProcessingRows(micProcessing.value) : [],
);
const audioProcessingPending = ref(false);
const audioProcessingDisabled = computed(
  () => micProcessing.value.kind !== "active" || audioProcessingPending.value,
);

/**
 * Opt-in client-side AI noise filter (#914). The selected model is persisted
 * in prefs; the *verified* row reads the attached processor, so it stays honest
 * even when an attach fails. Capability support is a one-time probe (AudioWorklet
 * presence doesn't change at runtime); DeepFilterNet is a deferred disabled slot.
 */
const aiNoiseSupport = noiseModelSupport(currentNoiseModelSupportEnv());
const aiNoiseControlEnabled = anyNoiseModelAvailable(aiNoiseSupport);
/** Per-model option view for the radiogroup — flattens the support union. */
const aiNoiseModelOptions = orderedNoiseModelMetas().map((meta) => {
  const support = aiNoiseSupport[meta.id];
  return {
    id: meta.id,
    label: meta.label,
    available: support.available,
    hint: support.available ? meta.costHint : support.reason,
  };
});
const aiNoiseModel = computed(() => prefs.value.aiNoiseModel);
const aiNoisePending = ref(false);
const aiNoiseError = useStore($aiNoiseFilterError);
const aiNoiseErrorLabel = computed(() =>
  aiNoiseError.value ? noiseModelMeta(aiNoiseError.value).name : null,
);
const aiFilterState = useStore($micAiNoiseFilter);
/** The model VERIFIABLY running on the live mic (null if none/failed/no mic). */
const verifiedAiModel = computed(() =>
  aiFilterState.value.kind === "active" ? aiFilterState.value.model : null,
);
/**
 * Drive the "browser NS is superseded" UI from the *verified* model, not the
 * pref: a selected-but-failed model must not claim to be handling noise
 * cancellation while browser NS is actually the only thing (and now stays) on.
 */
const aiFilterActive = computed(() => verifiedAiModel.value !== null);
const aiFilterRow = computed(() =>
  aiFilterState.value.kind === "active" ? aiNoiseFilterRow(aiFilterState.value) : null,
);

async function selectAiNoiseModel(model: NoiseModelId | null): Promise<void> {
  // Skip only when this model is both selected AND verifiably attached;
  // otherwise allow a re-click to retry a selection that didn't take.
  if (aiNoiseModel.value === model && verifiedAiModel.value === model) return;
  aiNoisePending.value = true;
  try {
    await applyAiNoiseModelSelection(model, engine);
  } catch (err) {
    reportCallError(err);
  } finally {
    aiNoisePending.value = false;
  }
}

/**
 * Opt-in virtual background / blur (#1024). The selected effect is persisted in
 * prefs; the *verified* state reads the attached processor, so a re-click can
 * retry a selection that didn't take. Capability is a one-time probe (the WebGL2
 * + frame-pipeline support doesn't change at runtime).
 */
const backgroundSupport = backgroundEffectSupport(currentBackgroundEffectSupportEnv());
const backgroundControlEnabled = backgroundSupport.available;
const backgroundUnavailableReason = backgroundSupport.available ? null : backgroundSupport.reason;
const backgroundCatalogEntries = backgroundCatalog();
const BLUR_EFFECT: BackgroundEffect = { kind: "blur" };

const backgroundEffect = computed(() => prefs.value.backgroundEffect);
const backgroundPending = ref(false);
const backgroundError = useStore($backgroundEffectError);
const backgroundErrorLabel = computed(() => {
  const effect = backgroundError.value;
  if (!effect) return null;
  return effect.kind === "blur" ? "background blur" : "background image";
});
const cameraBackground = useStore($cameraBackground);
/** The effect VERIFIABLY running on the live camera (null if none/failed/no camera). */
const verifiedBackground = computed(() =>
  cameraBackground.value.kind === "active" ? cameraBackground.value.effect : null,
);

function isBackgroundSelected(effect: BackgroundEffect): boolean {
  return sameBackgroundEffect(backgroundEffect.value, effect);
}
function isCatalogSelected(id: BackgroundImageId): boolean {
  return isBackgroundSelected({ kind: "image", image: { source: "catalog", id } });
}
const isCustomSelected = computed(
  () => backgroundEffect.value.kind === "image" && backgroundEffect.value.image.source === "custom",
);

async function selectBackground(effect: BackgroundEffect): Promise<void> {
  // Skip only when this effect is both selected AND verifiably attached;
  // otherwise allow a re-click to retry a selection that didn't take.
  const verified = verifiedBackground.value;
  if (isBackgroundSelected(effect) && verified && sameBackgroundEffect(verified, effect)) return;
  backgroundPending.value = true;
  try {
    await applyBackgroundEffectSelection(effect, engine);
  } catch (err) {
    reportCallError(err);
  } finally {
    backgroundPending.value = false;
  }
}

async function uploadCustomBackground(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = ""; // let the user re-pick the same file later
  if (!file) return;
  backgroundPending.value = true;
  try {
    const ref = await saveCustomBackground(file);
    await applyBackgroundEffectSelection(
      { kind: "image", image: { source: "custom", ref } },
      engine,
    );
  } catch (err) {
    reportCallError(err);
  } finally {
    backgroundPending.value = false;
  }
}

/**
 * Epoch counter incremented on every dialog close. `refresh()` reads
 * the current epoch before awaiting `enumerateCallDevices()` and
 * compares it against the snapshot afterward: if they differ, the
 * dialog has been closed (or re-opened) in the meantime and we drop
 * the stale result instead of overwriting whatever the next session
 * already set. This is the lighter-weight equivalent of an
 * AbortController — the underlying `enumerateDevices` call has no
 * abort signal anyway, so the epoch guard is the surgical fix.
 */
let enumerateEpoch = 0;

async function refresh(): Promise<void> {
  // The browser only populates `label` after the user has granted
  // permission for at least one input device. Calling `getUserMedia`
  // with empty constraints first would force a prompt — too
  // intrusive here. So before granting: labels are blank, and we
  // render a "Grant access to see device names" hint inside each
  // picker. The deviceId column still works for selection.
  const epoch = enumerateEpoch;
  const next = await enumerateCallDevices();
  if (epoch !== enumerateEpoch) return;
  devices.value = next;
}

const { engine, remoteTracks, localTracks } = useCallEngine();

/**
 * Live call diagnostics. Polls each published (send) and subscribed
 * (recv) video track's `getRTCStatsReport()` once per second — but ONLY
 * while this dialog is open (started/stopped in the `open` watcher), so a
 * normal call pays zero stats cost. `statSamples` carries the previous
 * byte/timestamp pair per track so bitrate can be derived as a rate.
 */
const statRows = ref<CallStatRow[]>([]);
const statSamples = new Map<string, CallStatSample>();
let statsTimer: ReturnType<typeof setInterval> | null = null;
// Guards against overlapping ticks: a slow `getStats()` must not let the
// next interval fire a second concurrent pass (which would diff against a
// half-updated sample map and could write rows out of order).
let statsSampling = false;
// Bumped every start/stop. A tick captures the generation at launch and
// re-checks it after its awaits before committing any sample or row, so an
// in-flight pass that outlives a close/reopen (or a switch to another call)
// can never repopulate state for a session that has already torn down.
let statsGeneration = 0;

const statDisplayRows = computed(() =>
  statRows.value.map((row) => ({
    key: row.key,
    label: row.label,
    sourceLabel: row.sourceLabel,
    ...describeVideoStats(row),
  })),
);

function participantShortLabel(identity: string): string {
  const at = identity.indexOf("@");
  const local = at > 0 ? identity.slice(0, at) : identity;
  return local || identity || "Participant";
}

function sourceLabel(source: string): string {
  return source === "screen_share" ? "Screen" : "Camera";
}

/** One track's sampled row plus the fresh byte/timestamp sample to retain. */
type TrackSample = { row: CallStatRow; key: string; sample: CallStatSample | null };

/**
 * Read and summarize one video track's stats, or `null` when the report
 * is unavailable. `getRTCStatsReport()` rejects when a sender/receiver is
 * mid-teardown (track unpublished, ICE restart, peer leaving) — common
 * exactly while a poll is running — so a failure drops that single row
 * rather than rejecting the whole tick.
 *
 * Pure read: the fresh `sample` is RETURNED, not written to `statSamples`
 * here, so the caller can commit it atomically only after re-checking the
 * polling generation (a stale in-flight pass must not mutate the map).
 */
async function sampleTrack(
  track: { track: { getRTCStatsReport(): Promise<RTCStatsReport | undefined> } },
  key: string,
  direction: "send" | "recv",
  label: string,
  source: string,
): Promise<TrackSample | null> {
  try {
    const report = await track.track.getRTCStatsReport();
    if (!report) return null;
    const { summary, sample } = summarizeVideoStats(report, direction, statSamples.get(key));
    return { row: { ...summary, key, label, sourceLabel: sourceLabel(source), direction }, key, sample };
  } catch {
    return null;
  }
}

async function sampleStats(generation: number): Promise<void> {
  if (statsSampling) return;
  statsSampling = true;
  try {
    const local = localTracks.value.filter((track) => track.kind === "video");
    const remote = remoteTracks.value.filter((track) => track.kind === "video");
    const results = (
      await Promise.all([
        ...local.map((track) =>
          sampleTrack(track, `local:${track.source}:${track.publicationSid}`, "send", "You", track.source),
        ),
        ...remote.map((track) =>
          sampleTrack(
            track,
            `remote:${track.participantIdentity}:${track.source}:${track.publicationSid}`,
            "recv",
            participantShortLabel(track.participantIdentity),
            track.source,
          ),
        ),
      ])
    ).filter((result): result is TrackSample => result !== null);
    // Bail if polling was stopped/restarted while we awaited: this pass
    // belongs to a torn-down session and must not touch live state.
    if (generation !== statsGeneration) return;
    // Commit samples + rows together, now that the generation is confirmed.
    for (const result of results) {
      if (result.sample) statSamples.set(result.key, result.sample);
    }
    const rows = results.map((result) => result.row);
    // Stable order: self first, then remotes alphabetically, so rows don't
    // jump between polls.
    rows.sort((a, b) =>
      a.direction === b.direction
        ? a.label.localeCompare(b.label)
        : a.direction === "send"
          ? -1
          : 1,
    );
    statRows.value = rows;
  } finally {
    statsSampling = false;
  }
}

function startStatsPolling(): void {
  stopStatsPolling();
  const generation = ++statsGeneration;
  statsTimer = setInterval(() => void sampleStats(generation), 1000);
  void sampleStats(generation);
}

function stopStatsPolling(): void {
  // Invalidate any in-flight tick so its post-await commit no-ops.
  statsGeneration += 1;
  if (statsTimer) {
    clearInterval(statsTimer);
    statsTimer = null;
  }
  statSamples.clear();
  statRows.value = [];
}

// A dialog unmounted while still open must not leak its interval.
onScopeDispose(stopStatsPolling);

async function selectMic(id: string | null): Promise<void> {
  try {
    await applyCallDeviceSelection("mic", id, engine);
  } catch (err) {
    reportCallError(err);
  }
}

async function selectCam(id: string | null): Promise<void> {
  try {
    await applyCallDeviceSelection("cam", id, engine);
  } catch (err) {
    reportCallError(err);
  }
}

async function selectSpeaker(id: string | null): Promise<void> {
  try {
    await applyCallDeviceSelection("speaker", id, engine);
  } catch (err) {
    reportCallError(err);
  }
}

async function setAudioProcessing(
  key: keyof AudioProcessingPrefs,
  enabled: boolean,
  event: Event,
): Promise<void> {
  const input = event.target instanceof HTMLInputElement ? event.target : null;
  const previous = prefs.value.audioProcessing[key];
  audioProcessingPending.value = true;
  try {
    await applyAudioProcessingSelection(
      { ...prefs.value.audioProcessing, [key]: enabled },
      engine,
    );
  } catch (err) {
    if (input) input.checked = previous;
    reportCallError(err);
  } finally {
    audioProcessingPending.value = false;
  }
}

/**
 * `devicechange` listener wired up only while the dialog is open.
 * Previously this was mounted for the entire call lifetime, which
 * meant a hot-plug during the call would re-enumerate even when the
 * picker wasn't visible — pointless work and an extra permission
 * probe on some browsers. Scoping to the open watcher keeps the call
 * surface idle when the user isn't actively choosing a device.
 */
let unsubscribeDeviceChange: (() => void) | null = null;
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

function teardownDeviceChange(): void {
  if (debounceTimer) {
    clearTimeout(debounceTimer);
    debounceTimer = null;
  }
  unsubscribeDeviceChange?.();
  unsubscribeDeviceChange = null;
}

watch(open, async (isOpen) => {
  if (!isOpen) {
    // Closing: bump the epoch so any in-flight enumerate result
    // gets dropped on resolve, and detach the devicechange listener.
    enumerateEpoch += 1;
    teardownDeviceChange();
    stopStatsPolling();
    return;
  }
  await refresh();
  // `refresh()` awaited device enumeration; if the dialog was closed in
  // the meantime, bail before wiring up listeners/polling — otherwise a
  // closed dialog would keep a 1s `getStats()` interval (and the
  // devicechange listener) running until the next open→close cycle.
  if (!open.value) return;
  // Begin the 1s diagnostics poll now that the dialog is visible; it
  // tears down again on close / unmount.
  startStatsPolling();
  // Mount the devicechange listener after the first refresh so the
  // initial enumeration isn't fighting a hot-plug event arriving in
  // the same tick. Debounced 200ms because hot-plugs commonly fire
  // multiple `devicechange` events in quick succession (one per
  // device flavor) and we only need one re-enumerate per cluster.
  if (typeof navigator !== "undefined" && navigator.mediaDevices?.addEventListener) {
    const handler = () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => {
        debounceTimer = null;
        void refresh();
      }, 200);
    };
    navigator.mediaDevices.addEventListener("devicechange", handler);
    unsubscribeDeviceChange = () => {
      navigator.mediaDevices.removeEventListener("devicechange", handler);
    };
  }
});

function close(): void {
  open.value = false;
}

// Huddle-Settings board: sections split by hairlines, kicker subheads,
// switch rows, background tiles and a mono diagnostics readout.
const bodyClass = cx(dialogParts.body, css({ gap: 0, paddingTop: 0, paddingBottom: 0, color: "fg" }));
const sectionClass = css({
  display: "flex",
  flexDirection: "column",
  gap: "2.5",
  padding: "18px 0",
  borderTop: "1px solid {colors.border.soft}",
  "& h3": { margin: 0, display: "flex", alignItems: "center", gap: "2", font: "600 15px/1.3 {fonts.display}" },
  "& h3 svg": { width: "16px", height: "16px", color: "fg.muted" },
});
const subheadClass = cx(kicker(), css({ margin: "4px 0 0" }));
const captionClass = css({ margin: 0, fontSize: "13px", lineHeight: "1.45", color: "fg.muted" });
const errorClass = css({ margin: 0, fontSize: "13px", lineHeight: "1.45", color: "danger.text" });
const listClass = css({ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: "0.5" });
const deviceRowClass = css({
  display: "flex",
  width: "100%",
  alignItems: "center",
  justifyContent: "space-between",
  gap: "3",
  minHeight: "40px",
  padding: "8px 12px",
  border: "1px solid transparent",
  borderRadius: "control",
  bg: "transparent",
  color: "fg",
  font: "400 14px/1.3 {fonts.body}",
  textAlign: "start",
  cursor: "pointer",
  _hover: { bg: "state.hover" },
  _disabled: { opacity: 0.55, cursor: "not-allowed" },
  '&[aria-checked="true"]': { bg: "state.selected", borderColor: "border" },
  "& > span": { minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  "& > svg": { width: "16px", height: "16px", flex: "none", color: "action.text" },
});
const optionCopyClass = css({ display: "flex", flexDirection: "column", gap: "0.5", minWidth: 0, "& > span:last-child": { fontSize: "12px", color: "fg.muted" } });
const fieldsetClass = css({ border: 0, margin: 0, padding: 0, minWidth: 0, display: "flex", flexDirection: "column", gap: "1", _disabled: { opacity: 0.6 } });
const legendClass = cx(kicker(), css({ padding: 0, marginBottom: "1" }));
const switchRowClass = css({ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "3", minHeight: "40px", fontSize: "14px", cursor: "pointer" });
const switchCopyClass = css({ display: "flex", flexDirection: "column", gap: "0.5", minWidth: 0, "& > span + span": { fontSize: "12px", color: "fg.muted" } });
// A native checkbox drawn as the preset switch (track + thumb).
const switchClass = css({
  appearance: "none",
  flex: "none",
  width: "44px",
  height: "26px",
  margin: 0,
  boxSizing: "border-box",
  borderRadius: "pill",
  border: "1px solid {colors.border.strong}",
  bg: "surface.2",
  backgroundImage: "radial-gradient(circle at 12px 50%, {colors.fg.muted} 9px, transparent 10px)",
  cursor: "pointer",
  transition: "background-color {durations.base} {easings.out}, background-position {durations.slow} {easings.spring.snappy}",
  _checked: {
    bg: "action",
    borderColor: "action",
    backgroundImage: "radial-gradient(circle at 30px 50%, {colors.action.fg} 9px, transparent 10px)",
  },
  _disabled: { opacity: 0.5, cursor: "not-allowed" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
  "@media (forced-colors: active)": { appearance: "auto" },
});
const processingListClass = css({ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: "2" });
const processingRowClass = css({ display: "flex", flexDirection: "column", gap: "0.5", fontSize: "13px" });
const processingHeadClass = css({ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "3" });
const stateBadgeClass = css({
  borderRadius: "tag",
  padding: "1px 8px",
  font: "600 12px/1.5 {fonts.body}",
  whiteSpace: "nowrap",
  fontVariantNumeric: "tabular-nums",
  bg: "surface.2",
  color: "fg.muted",
  '&[data-tone="on"]': { bg: "success.subtle", color: "success.text" },
  '&[data-tone="warn"]': { bg: "warning.subtle", color: "warning.text" },
});
const groupClass = css({ display: "flex", flexDirection: "column", gap: "2", marginTop: "2" });
const tilesClass = css({ listStyle: "none", margin: 0, padding: 0, display: "flex", flexWrap: "wrap", gap: "2" });
const tileClass = css({
  position: "relative",
  display: "inline-flex",
  flexDirection: "column",
  alignItems: "center",
  justifyContent: "center",
  gap: "0.5",
  width: "64px",
  height: "44px",
  boxSizing: "border-box",
  borderRadius: "10px",
  border: "1px solid {colors.border}",
  bg: "surface.2",
  backgroundSize: "cover",
  backgroundPosition: "center",
  color: "fg.muted",
  fontSize: "12px",
  cursor: "pointer",
  overflow: "hidden",
  _disabled: { opacity: 0.6, cursor: "not-allowed" },
  '&:is([aria-checked="true"], [data-selected])': { boxShadow: "0 0 0 2px {colors.action}", color: "fg" },
  "&:has(input:focus-visible)": { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
  "& svg": { width: "16px", height: "16px" },
});
const tileCheckClass = css({ position: "absolute", top: "2px", right: "2px", borderRadius: "pill", bg: "surface", color: "action.text", padding: "1px" });
const srOnlyClass = css({ srOnly: true });
const statsClass = css({ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column" });
const statRowClass = css({ display: "flex", flexDirection: "column", gap: "1", padding: "8px 0", borderTop: "1px solid {colors.border.soft}", _first: { borderTop: 0 } });
const statHeadClass = css({ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: "3", fontSize: "13px" });
const statMonoClass = css({ display: "flex", flexWrap: "wrap", gap: "1 3", font: "400 13px/1.5 {fonts.mono}", fontVariantNumeric: "tabular-nums", color: "fg.muted" });
const footClass = cx(dialogParts.foot, css({ paddingTop: "14px", paddingBottom: "14px", borderTop: "1px solid {colors.border.soft}" }));
</script>

<template>
  <AppDialog v-model:open="open" labelled-by="call-settings-title">
    <header :class="dialogParts.head">
      <h2 id="call-settings-title">Call settings</h2>
      <button
        type="button"
        :class="iconbtn()"
        aria-label="Close settings"
        @click="close"
      >
        <X aria-hidden="true" />
      </button>
    </header>

    <div :class="bodyClass">
      <!-- Microphone -->
      <section :class="sectionClass">
        <h3><Mic aria-hidden="true" />Microphone</h3>
        <div v-if="devices.mics.length === 0" :class="captionClass">
          No microphones detected. Grant microphone access in your browser to
          see available devices.
        </div>
        <ul v-else :class="listClass" role="radiogroup" aria-label="Microphone">
          <li>
            <button
              type="button"
              :class="deviceRowClass"
              role="radio"
              :aria-checked="prefs.mic === null"
              @click="selectMic(null)"
            >
              <span>System default</span>
              <Check v-if="prefs.mic === null" aria-hidden="true" />
            </button>
          </li>
          <li v-for="device in devices.mics" :key="device.deviceId">
            <button
              type="button"
              :class="deviceRowClass"
              role="radio"
              :aria-checked="prefs.mic === device.deviceId"
              @click="selectMic(device.deviceId)"
            >
              <span>{{ device.label || `Microphone (${device.deviceId.slice(0, 6)}…)` }}</span>
              <Check v-if="prefs.mic === device.deviceId" aria-hidden="true" />
            </button>
          </li>
        </ul>

        <!-- Read-only verification of the audio processing the browser
             actually applied to the live mic (not what we requested). -->
        <div :class="groupClass">
          <fieldset
            :class="fieldsetClass"
            :aria-describedby="micProcessing.kind === 'no-mic' ? 'call-processing-no-mic' : undefined"
            :disabled="audioProcessingDisabled"
          >
            <legend :class="legendClass">
              Requested audio processing
            </legend>
            <label :class="switchRowClass">
              <span :class="switchCopyClass">
                <span>Noise cancellation</span>
                <span
                  v-if="aiFilterActive"
                >
                  Handled by the AI filter
                </span>
              </span>
              <input
                :class="switchClass"
                type="checkbox"
                :checked="aiFilterActive ? false : prefs.audioProcessing.noiseSuppression"
                :disabled="aiFilterActive"
                @change="setAudioProcessing('noiseSuppression', ($event.target as HTMLInputElement).checked, $event)"
              />
            </label>
            <label :class="switchRowClass">
              <span :class="switchCopyClass">
                <span>Echo cancellation</span>
              </span>
              <input
                :class="switchClass"
                type="checkbox"
                :checked="prefs.audioProcessing.echoCancellation"
                @change="setAudioProcessing('echoCancellation', ($event.target as HTMLInputElement).checked, $event)"
              />
            </label>
            <label :class="switchRowClass">
              <span :class="switchCopyClass">
                <span>Auto gain control</span>
              </span>
              <input
                :class="switchClass"
                type="checkbox"
                :checked="prefs.audioProcessing.autoGainControl"
                @change="setAudioProcessing('autoGainControl', ($event.target as HTMLInputElement).checked, $event)"
              />
            </label>
          </fieldset>
          <h4 :class="subheadClass">Audio processing</h4>
          <p
            v-if="micProcessing.kind === 'no-mic'"
            id="call-processing-no-mic"
            :class="captionClass"
          >
            No microphone active — enable your mic to verify audio
            processing.
          </p>
          <ul v-else :class="processingListClass">
            <li
              v-for="row in processingRows"
              :key="row.key"
              :class="processingRowClass"
            >
              <div :class="processingHeadClass">
                <span>{{ row.label }}</span>
                <span
                  :class="stateBadgeClass"
                  :data-tone="row.tone"
                >
                  {{ row.stateLabel }}
                </span>
              </div>
              <p
                v-if="row.detail"
                :class="captionClass"
              >
                {{ row.detail }}
              </p>
            </li>
          </ul>

          <!-- Opt-in client-side AI noise filter (#914). Runs entirely in the
               browser as a WASM AudioWorklet; defaults off. -->
          <div :class="groupClass">
            <h4 :class="subheadClass">AI noise filter</h4>
            <p
              v-if="!aiNoiseControlEnabled"
              :class="captionClass"
            >
              Not available in this browser.
            </p>
            <template v-else>
              <ul :class="listClass" role="radiogroup" aria-label="AI noise filter">
                <li>
                  <button
                    type="button"
                    :class="deviceRowClass"
                    role="radio"
                    :aria-checked="aiNoiseModel === null"
                    :disabled="aiNoisePending"
                    @click="selectAiNoiseModel(null)"
                  >
                    <span>Off</span>
                    <Check v-if="aiNoiseModel === null" aria-hidden="true" />
                  </button>
                </li>
                <li v-for="model in aiNoiseModelOptions" :key="model.id">
                  <button
                    type="button"
                    :class="deviceRowClass"
                    role="radio"
                    :aria-checked="aiNoiseModel === model.id"
                    :disabled="aiNoisePending || !model.available"
                    @click="selectAiNoiseModel(model.id)"
                  >
                    <span :class="optionCopyClass">
                      <span>{{ model.label }}</span>
                      <span>{{ model.hint }}</span>
                    </span>
                    <Check v-if="aiNoiseModel === model.id" aria-hidden="true" />
                  </button>
                </li>
              </ul>
              <p :class="captionClass">
                Runs entirely in your browser. Higher settings remove more noise
                but use more CPU.
              </p>
              <p
                v-if="aiNoiseErrorLabel"
                :class="errorClass"
              >
                Couldn't start the {{ aiNoiseErrorLabel }} filter — using your raw mic.
              </p>
              <div v-if="aiFilterRow" :class="processingRowClass">
                <div :class="processingHeadClass">
                  <span>{{ aiFilterRow.label }}</span>
                  <span
                    :class="stateBadgeClass"
                    :data-tone="aiFilterRow.tone"
                  >
                    {{ aiFilterRow.stateLabel }}
                  </span>
                </div>
              </div>
            </template>
          </div>
        </div>
      </section>

      <!-- Camera -->
      <section :class="sectionClass">
        <h3><Video aria-hidden="true" />Camera</h3>
        <div v-if="devices.cams.length === 0" :class="captionClass">
          No cameras detected. Grant camera access in your browser to see
          available devices.
        </div>
        <ul v-else :class="listClass" role="radiogroup" aria-label="Camera">
          <li>
            <button
              type="button"
              :class="deviceRowClass"
              role="radio"
              :aria-checked="prefs.cam === null"
              @click="selectCam(null)"
            >
              <span>System default</span>
              <Check v-if="prefs.cam === null" aria-hidden="true" />
            </button>
          </li>
          <li v-for="device in devices.cams" :key="device.deviceId">
            <button
              type="button"
              :class="deviceRowClass"
              role="radio"
              :aria-checked="prefs.cam === device.deviceId"
              @click="selectCam(device.deviceId)"
            >
              <span>{{ device.label || `Camera (${device.deviceId.slice(0, 6)}…)` }}</span>
              <Check v-if="prefs.cam === device.deviceId" aria-hidden="true" />
            </button>
          </li>
        </ul>

        <!-- Opt-in virtual background / blur (#1024). MediaPipe segmentation
             runs in the browser via @livekit/track-processors; defaults off. -->
        <div :class="groupClass">
          <h4 :class="subheadClass">Background</h4>
          <p v-if="!backgroundControlEnabled" :class="captionClass">
            {{ backgroundUnavailableReason }}
          </p>
          <template v-else>
            <div :class="tilesClass">
            <ul :class="tilesClass" role="radiogroup" aria-label="Camera background">
              <li>
                <button
                  type="button"
                  :class="tileClass"
                  role="radio"
                  :aria-checked="isBackgroundSelected(BACKGROUND_OFF)"
                  :disabled="backgroundPending"
                  @click="selectBackground(BACKGROUND_OFF)"
                >
                  <span>Off</span>
                </button>
              </li>
              <li>
                <button
                  type="button"
                  :class="tileClass"
                  role="radio"
                  :aria-checked="isBackgroundSelected(BLUR_EFFECT)"
                  :disabled="backgroundPending"
                  @click="selectBackground(BLUR_EFFECT)"
                >
                  <span>Blur</span>
                </button>
              </li>
            </ul>
            <ul :class="tilesClass" role="radiogroup" aria-label="Background image">
              <li v-for="entry in backgroundCatalogEntries" :key="entry.id">
                <button
                  type="button"
                  :class="tileClass"
                  role="radio"
                  :aria-checked="isCatalogSelected(entry.id)"
                  :aria-label="entry.label"
                  :disabled="backgroundPending"
                  :style="{ backgroundImage: `url(${entry.assetPath})` }"
                  @click="selectBackground({ kind: 'image', image: { source: 'catalog', id: entry.id } })"
                >
                  <Check v-if="isCatalogSelected(entry.id)" :class="tileCheckClass" aria-hidden="true" />
                </button>
              </li>
              <li>
                <label
                  :class="tileClass"
                  :data-selected="isCustomSelected || undefined"
                >
                  <input
                    type="file"
                    accept="image/*"
                    :class="srOnlyClass"
                    :disabled="backgroundPending"
                    @change="uploadCustomBackground"
                  />
                  <ImagePlus aria-hidden="true" />
                  <span>Upload</span>
                  <Check v-if="isCustomSelected" :class="tileCheckClass" aria-hidden="true" />
                </label>
              </li>
            </ul>
            </div>
            <p :class="captionClass">
              Runs entirely in your browser. Your camera image is processed on
              your device before it's sent.
            </p>
            <p v-if="backgroundErrorLabel" :class="errorClass">
              Couldn't start the {{ backgroundErrorLabel }} — using your raw camera.
            </p>
          </template>
        </div>
      </section>

      <!-- Speaker -->
      <section :class="sectionClass">
        <h3><Speaker aria-hidden="true" />Speaker</h3>
        <div v-if="!speakerSupported" :class="captionClass">
          Your browser doesn't support choosing the speaker device. Set the
          default output in your operating system instead.
        </div>
        <div
          v-else-if="devices.speakers.length === 0"
          :class="captionClass"
        >
          No audio outputs detected. Grant microphone access first — most
          browsers reveal speakers only after at least one media permission is
          granted.
        </div>
        <ul
          v-else
          :class="listClass"
          role="radiogroup"
          aria-label="Speaker"
        >
          <li>
            <button
              type="button"
              :class="deviceRowClass"
              role="radio"
              :aria-checked="prefs.speaker === null"
              @click="selectSpeaker(null)"
            >
              <span>System default</span>
              <Check v-if="prefs.speaker === null" aria-hidden="true" />
            </button>
          </li>
          <li v-for="device in devices.speakers" :key="device.deviceId">
            <button
              type="button"
              :class="deviceRowClass"
              role="radio"
              :aria-checked="prefs.speaker === device.deviceId"
              @click="selectSpeaker(device.deviceId)"
            >
              <span>{{ device.label || `Speaker (${device.deviceId.slice(0, 6)}…)` }}</span>
              <Check v-if="prefs.speaker === device.deviceId" aria-hidden="true" />
            </button>
          </li>
        </ul>
      </section>

      <!-- Diagnostics: live per-track WebRTC stats. Polled only while
           this dialog is open (see the `open` watcher). -->
      <section :class="sectionClass">
        <h3><Activity aria-hidden="true" />Diagnostics</h3>
        <p
          v-if="statDisplayRows.length === 0"
          :class="captionClass"
        >
          No active video. Start your camera or screen share to see live
          resolution, bitrate, and packet-loss stats.
        </p>
        <ul v-else :class="statsClass">
          <li v-for="row in statDisplayRows" :key="row.key" :class="statRowClass">
            <div :class="statHeadClass">
              <span>
                {{ row.label }} · {{ row.sourceLabel }}
              </span>
              <span :class="statMonoClass">
                {{ row.resolution }} · {{ row.fps }}
              </span>
            </div>
            <div :class="statMonoClass">
              <span>{{ row.bitrate }}</span>
              <span>loss {{ row.loss }}</span>
              <span>{{ row.rtt }}</span>
              <span>{{ row.codec }}</span>
              <span>{{ row.icePath }}</span>
            </div>
          </li>
        </ul>
      </section>
    </div>

    <footer :class="footClass">
      <button type="button" :class="btn({ variant: 'primary' })" @click="close">
        Done
      </button>
    </footer>
  </AppDialog>
</template>
