<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Activity, Check, Mic, Speaker, Video, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import {
  $devicePrefs,
  enumerateCallDevices,
  isSpeakerOutputSelectionSupported,
  setCamDevice,
  setMicDevice,
  setSpeakerDevice,
  type EnumeratedDevices,
} from "@/lib/calls/device-prefs";
import { $micAudioProcessing } from "@/lib/calls/mic-audio-processing-state";
import { audioProcessingRows } from "@/lib/calls/mic-audio-processing";
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
 * Live call diagnostics. Polls `getRTCStatsReport()` for every published
 * (send) and subscribed (recv) video track once per second — but ONLY
 * while this dialog is open (started/stopped in the `open` watcher), so a
 * normal call pays zero stats cost. `statSamples` carries the previous
 * byte/timestamp pair per track so bitrate can be derived as a rate.
 */
const statRows = ref<CallStatRow[]>([]);
const statSamples = new Map<string, CallStatSample>();
let statsTimer: ReturnType<typeof setInterval> | null = null;

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
  return local || identity;
}

function sourceLabel(source: string): string {
  return source === "screen_share" ? "Screen" : "Camera";
}

async function sampleStats(): Promise<void> {
  const local = localTracks.value.filter((track) => track.kind === "video");
  const remote = remoteTracks.value.filter((track) => track.kind === "video");
  const rows: CallStatRow[] = [];
  await Promise.all([
    ...local.map(async (track) => {
      const key = `local:${track.source}:${track.publicationSid}`;
      const report = await track.track.getRTCStatsReport();
      if (!report) return;
      const { summary, sample } = summarizeVideoStats(report, "send", statSamples.get(key));
      if (sample) statSamples.set(key, sample);
      rows.push({
        ...summary,
        key,
        label: "You",
        sourceLabel: sourceLabel(track.source),
        direction: "send",
      });
    }),
    ...remote.map(async (track) => {
      const key = `remote:${track.participantIdentity}:${track.source}:${track.publicationSid}`;
      const report = await track.track.getRTCStatsReport();
      if (!report) return;
      const { summary, sample } = summarizeVideoStats(report, "recv", statSamples.get(key));
      if (sample) statSamples.set(key, sample);
      rows.push({
        ...summary,
        key,
        label: participantShortLabel(track.participantIdentity),
        sourceLabel: sourceLabel(track.source),
        direction: "recv",
      });
    }),
  ]);
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
}

function startStatsPolling(): void {
  stopStatsPolling();
  void sampleStats();
  statsTimer = setInterval(() => void sampleStats(), 1000);
}

function stopStatsPolling(): void {
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
  setMicDevice(id);
  if (id) {
    try {
      await engine.setMicDevice(id);
    } catch (err) {
      reportCallError(err);
    }
  }
}

async function selectCam(id: string | null): Promise<void> {
  setCamDevice(id);
  if (id) {
    try {
      await engine.setCameraDevice(id);
    } catch (err) {
      reportCallError(err);
    }
  }
}

async function selectSpeaker(id: string | null): Promise<void> {
  setSpeakerDevice(id);
  if (id) {
    try {
      await engine.setSpeakerDevice(id);
    } catch (err) {
      reportCallError(err);
    }
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
          <h4 class="type-caption call-processing__title">Audio processing</h4>
          <p
            v-if="micProcessing.kind === 'no-mic'"
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
