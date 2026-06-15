<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Activity, Check, Mic, Speaker, Video, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
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
</script>

<template>
  <AppDialog v-model:open="open">
    <header class="chat-dialog-header">
      <h2 class="type-chat-title">Call settings</h2>
      <button
        type="button"
        class="chat-icon-button chat-icon-button--md hover:bg-muted"
        aria-label="Close settings"
        @click="close"
      >
        <X class="w-4 h-4" />
      </button>
    </header>

    <div class="chat-dialog-body flex flex-col gap-4 overflow-y-auto">
      <!-- Microphone -->
      <section class="chat-section-card">
        <div class="flex items-center gap-2 mb-2">
          <Mic class="w-4 h-4 text-muted-foreground" />
          <h3 class="type-control">Microphone</h3>
        </div>
        <div v-if="devices.mics.length === 0" class="type-caption text-muted-foreground">
          No microphones detected. Grant microphone access in your browser to
          see available devices.
        </div>
        <ul v-else class="chat-list-stack" role="radiogroup" aria-label="Microphone">
          <li>
            <button
              type="button"
              class="call-device-row"
              :class="prefs.mic === null ? 'call-device-row--active' : ''"
              role="radio"
              :aria-checked="prefs.mic === null"
              @click="selectMic(null)"
            >
              <span class="truncate">System default</span>
              <Check v-if="prefs.mic === null" class="w-4 h-4 text-primary" />
            </button>
          </li>
          <li v-for="device in devices.mics" :key="device.deviceId">
            <button
              type="button"
              class="call-device-row"
              :class="prefs.mic === device.deviceId ? 'call-device-row--active' : ''"
              role="radio"
              :aria-checked="prefs.mic === device.deviceId"
              @click="selectMic(device.deviceId)"
            >
              <span class="truncate">{{ device.label || `Microphone (${device.deviceId.slice(0, 6)}…)` }}</span>
              <Check v-if="prefs.mic === device.deviceId" class="w-4 h-4 text-primary" />
            </button>
          </li>
        </ul>

        <!-- Read-only verification of the audio processing the browser
             actually applied to the live mic (not what we requested). -->
        <div class="call-processing">
          <fieldset
            class="call-processing-controls"
            :aria-describedby="micProcessing.kind === 'no-mic' ? 'call-processing-no-mic' : undefined"
            :disabled="audioProcessingDisabled"
          >
            <legend class="type-caption call-processing__title">
              Requested audio processing
            </legend>
            <label class="call-processing-toggle">
              <span class="call-processing-toggle__copy">
                <span class="type-caption call-processing-toggle__label">Noise cancellation</span>
                <span
                  v-if="aiFilterActive"
                  class="type-caption text-muted-foreground call-processing-toggle__hint"
                >
                  Handled by the AI filter
                </span>
              </span>
              <input
                class="call-processing-toggle__input"
                type="checkbox"
                :checked="aiFilterActive ? false : prefs.audioProcessing.noiseSuppression"
                :disabled="aiFilterActive"
                @change="setAudioProcessing('noiseSuppression', ($event.target as HTMLInputElement).checked, $event)"
              />
            </label>
            <label class="call-processing-toggle">
              <span class="call-processing-toggle__copy">
                <span class="type-caption call-processing-toggle__label">Echo cancellation</span>
              </span>
              <input
                class="call-processing-toggle__input"
                type="checkbox"
                :checked="prefs.audioProcessing.echoCancellation"
                @change="setAudioProcessing('echoCancellation', ($event.target as HTMLInputElement).checked, $event)"
              />
            </label>
            <label class="call-processing-toggle">
              <span class="call-processing-toggle__copy">
                <span class="type-caption call-processing-toggle__label">Auto gain control</span>
              </span>
              <input
                class="call-processing-toggle__input"
                type="checkbox"
                :checked="prefs.audioProcessing.autoGainControl"
                @change="setAudioProcessing('autoGainControl', ($event.target as HTMLInputElement).checked, $event)"
              />
            </label>
          </fieldset>
          <h4 class="type-caption call-processing__title">Audio processing</h4>
          <p
            v-if="micProcessing.kind === 'no-mic'"
            id="call-processing-no-mic"
            class="type-caption text-muted-foreground"
          >
            No microphone active — enable your mic to verify audio
            processing.
          </p>
          <ul v-else class="call-processing__list">
            <li
              v-for="row in processingRows"
              :key="row.key"
              class="call-processing__row"
            >
              <div class="call-processing__head">
                <span class="type-caption">{{ row.label }}</span>
                <span
                  class="call-processing__badge"
                  :class="`call-processing__badge--${row.tone}`"
                >
                  {{ row.stateLabel }}
                </span>
              </div>
              <p
                v-if="row.detail"
                class="type-caption text-muted-foreground call-processing__detail"
              >
                {{ row.detail }}
              </p>
            </li>
          </ul>

          <!-- Opt-in client-side AI noise filter (#914). Runs entirely in the
               browser as a WASM AudioWorklet; defaults off. -->
          <div class="call-ai-filter">
            <h4 class="type-caption call-processing__title">AI noise filter</h4>
            <p
              v-if="!aiNoiseControlEnabled"
              class="type-caption text-muted-foreground"
            >
              Not available in this browser.
            </p>
            <template v-else>
              <ul class="chat-list-stack" role="radiogroup" aria-label="AI noise filter">
                <li>
                  <button
                    type="button"
                    class="call-device-row"
                    :class="aiNoiseModel === null ? 'call-device-row--active' : ''"
                    role="radio"
                    :aria-checked="aiNoiseModel === null"
                    :disabled="aiNoisePending"
                    @click="selectAiNoiseModel(null)"
                  >
                    <span class="truncate">Off</span>
                    <Check v-if="aiNoiseModel === null" class="w-4 h-4 text-primary" />
                  </button>
                </li>
                <li v-for="model in aiNoiseModelOptions" :key="model.id">
                  <button
                    type="button"
                    class="call-device-row"
                    :class="aiNoiseModel === model.id ? 'call-device-row--active' : ''"
                    role="radio"
                    :aria-checked="aiNoiseModel === model.id"
                    :disabled="aiNoisePending || !model.available"
                    @click="selectAiNoiseModel(model.id)"
                  >
                    <span class="call-ai-filter__option">
                      <span class="truncate">{{ model.label }}</span>
                      <span class="type-caption text-muted-foreground">{{ model.hint }}</span>
                    </span>
                    <Check v-if="aiNoiseModel === model.id" class="w-4 h-4 text-primary" />
                  </button>
                </li>
              </ul>
              <p class="type-caption text-muted-foreground call-ai-filter__note">
                Runs entirely in your browser. Higher settings remove more noise
                but use more CPU.
              </p>
              <p
                v-if="aiNoiseErrorLabel"
                class="type-caption call-ai-filter__error"
              >
                Couldn't start the {{ aiNoiseErrorLabel }} filter — using your raw mic.
              </p>
              <div v-if="aiFilterRow" class="call-processing__row">
                <div class="call-processing__head">
                  <span class="type-caption">{{ aiFilterRow.label }}</span>
                  <span
                    class="call-processing__badge"
                    :class="`call-processing__badge--${aiFilterRow.tone}`"
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
      <section class="chat-section-card">
        <div class="flex items-center gap-2 mb-2">
          <Video class="w-4 h-4 text-muted-foreground" />
          <h3 class="type-control">Camera</h3>
        </div>
        <div v-if="devices.cams.length === 0" class="type-caption text-muted-foreground">
          No cameras detected. Grant camera access in your browser to see
          available devices.
        </div>
        <ul v-else class="chat-list-stack" role="radiogroup" aria-label="Camera">
          <li>
            <button
              type="button"
              class="call-device-row"
              :class="prefs.cam === null ? 'call-device-row--active' : ''"
              role="radio"
              :aria-checked="prefs.cam === null"
              @click="selectCam(null)"
            >
              <span class="truncate">System default</span>
              <Check v-if="prefs.cam === null" class="w-4 h-4 text-primary" />
            </button>
          </li>
          <li v-for="device in devices.cams" :key="device.deviceId">
            <button
              type="button"
              class="call-device-row"
              :class="prefs.cam === device.deviceId ? 'call-device-row--active' : ''"
              role="radio"
              :aria-checked="prefs.cam === device.deviceId"
              @click="selectCam(device.deviceId)"
            >
              <span class="truncate">{{ device.label || `Camera (${device.deviceId.slice(0, 6)}…)` }}</span>
              <Check v-if="prefs.cam === device.deviceId" class="w-4 h-4 text-primary" />
            </button>
          </li>
        </ul>
      </section>

      <!-- Speaker -->
      <section class="chat-section-card">
        <div class="flex items-center gap-2 mb-2">
          <Speaker class="w-4 h-4 text-muted-foreground" />
          <h3 class="type-control">Speaker</h3>
        </div>
        <div v-if="!speakerSupported" class="type-caption text-muted-foreground">
          Your browser doesn't support choosing the speaker device. Set the
          default output in your operating system instead.
        </div>
        <div
          v-else-if="devices.speakers.length === 0"
          class="type-caption text-muted-foreground"
        >
          No audio outputs detected. Grant microphone access first — most
          browsers reveal speakers only after at least one media permission is
          granted.
        </div>
        <ul
          v-else
          class="chat-list-stack"
          role="radiogroup"
          aria-label="Speaker"
        >
          <li>
            <button
              type="button"
              class="call-device-row"
              :class="prefs.speaker === null ? 'call-device-row--active' : ''"
              role="radio"
              :aria-checked="prefs.speaker === null"
              @click="selectSpeaker(null)"
            >
              <span class="truncate">System default</span>
              <Check v-if="prefs.speaker === null" class="w-4 h-4 text-primary" />
            </button>
          </li>
          <li v-for="device in devices.speakers" :key="device.deviceId">
            <button
              type="button"
              class="call-device-row"
              :class="prefs.speaker === device.deviceId ? 'call-device-row--active' : ''"
              role="radio"
              :aria-checked="prefs.speaker === device.deviceId"
              @click="selectSpeaker(device.deviceId)"
            >
              <span class="truncate">{{ device.label || `Speaker (${device.deviceId.slice(0, 6)}…)` }}</span>
              <Check v-if="prefs.speaker === device.deviceId" class="w-4 h-4 text-primary" />
            </button>
          </li>
        </ul>
      </section>

      <!-- Diagnostics: live per-track WebRTC stats. Polled only while
           this dialog is open (see the `open` watcher). -->
      <section class="chat-section-card">
        <div class="flex items-center gap-2 mb-2">
          <Activity class="w-4 h-4 text-muted-foreground" />
          <h3 class="type-control">Diagnostics</h3>
        </div>
        <p
          v-if="statDisplayRows.length === 0"
          class="type-caption text-muted-foreground"
        >
          No active video. Start your camera or screen share to see live
          resolution, bitrate, and packet-loss stats.
        </p>
        <ul v-else class="call-stats">
          <li v-for="row in statDisplayRows" :key="row.key" class="call-stats__row">
            <div class="call-stats__head">
              <span class="type-caption call-stats__peer">
                {{ row.label }} · {{ row.sourceLabel }}
              </span>
              <span class="type-caption text-muted-foreground call-stats__res">
                {{ row.resolution }} · {{ row.fps }}
              </span>
            </div>
            <div class="call-stats__metrics type-caption text-muted-foreground">
              <span>{{ row.bitrate }}</span>
              <span>loss {{ row.loss }}</span>
              <span>{{ row.rtt }}</span>
            </div>
          </li>
        </ul>
      </section>
    </div>

    <footer class="chat-dialog-footer">
      <button type="button" class="chat-action-button chat-action-button--primary" @click="close">
        <span class="type-control">Done</span>
      </button>
    </footer>
  </AppDialog>
</template>

<style scoped>
.call-device-row {
  display: flex;
  width: 100%;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-sm);
  border-radius: var(--radius-sm);
  padding: 0.5rem 0.75rem;
  text-align: start;
  transition: background-color 0.18s ease;
}

.call-device-row:hover {
  background: var(--muted);
}

.call-device-row--active {
  background: color-mix(in oklab, var(--primary) 12%, transparent);
  color: var(--foreground);
}

.call-processing {
  margin-top: var(--space-sm);
  padding-top: var(--space-sm);
  border-top: 1px solid var(--muted);
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.call-processing__title {
  color: var(--muted-foreground);
}

.call-processing-controls {
  display: flex;
  flex-direction: column;
  gap: 0.375rem;
}

.call-processing-toggle {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-sm);
  min-height: 2rem;
}

.call-processing-toggle__copy {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
}

.call-processing-toggle__hint {
  font-style: italic;
}

.call-ai-filter {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  margin-top: 0.75rem;
}

.call-ai-filter__option {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
  min-width: 0;
}

.call-ai-filter__note {
  margin-top: -0.125rem;
}

.call-ai-filter__error {
  color: var(--destructive, #dc2626);
}

.call-processing-toggle__label {
  color: var(--foreground);
}

.call-processing-toggle__input {
  width: 2.25rem;
  height: 1.25rem;
  flex: 0 0 auto;
  accent-color: var(--primary);
}

.call-processing-toggle__input:disabled {
  opacity: 0.55;
}

.call-processing__list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.call-processing__row {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
}

.call-processing__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-sm);
}

.call-processing__badge {
  border-radius: var(--radius-sm);
  padding: 0.0625rem 0.5rem;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

/* On — calm positive: the requested processing is confirmed applied. */
.call-processing__badge--on {
  background: color-mix(in oklab, var(--success) 16%, transparent);
  color: var(--success-foreground);
}

/* Off — a genuine degradation the browser reported; worth noticing. */
.call-processing__badge--warn {
  background: color-mix(in oklab, var(--warning) 20%, transparent);
  color: var(--warning-foreground);
}

/* Unknown — browser doesn't report it; muted, neither pass nor fail. */
.call-processing__badge--muted {
  background: var(--muted);
  color: var(--muted-foreground);
}

.call-processing__detail {
  line-height: 1.3;
}

.call-stats {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.call-stats__row {
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
}

.call-stats__head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-sm);
}

.call-stats__peer {
  color: var(--foreground);
}

.call-stats__res {
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.call-stats__metrics {
  display: flex;
  gap: var(--space-sm);
  font-variant-numeric: tabular-nums;
}
</style>
