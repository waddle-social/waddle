<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Check, Mic, Speaker, Video, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import {
  $devicePrefs,
  enumerateCallDevices,
  setCamDevice,
  setMicDevice,
  setSpeakerDevice,
  type EnumeratedDevices,
} from "@/lib/calls/device-prefs";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { reportCallError } from "@/lib/calls/call-store";

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
 * Speaker selection requires `HTMLMediaElement.setSinkId`, which
 * is unavailable on Firefox today; we surface that as a disabled
 * picker row with explanatory copy rather than hiding the section.
 */
const open = defineModel<boolean>("open", { required: true });

const prefs = useStore($devicePrefs);
const devices = ref<EnumeratedDevices>({ mics: [], cams: [], speakers: [] });
const speakerSupported = ref(true);

function detectSpeakerSupport(): boolean {
  if (typeof window === "undefined") return false;
  const audio = document.createElement("audio") as HTMLAudioElement &
    Partial<{ setSinkId: (id: string) => Promise<void> }>;
  return typeof audio.setSinkId === "function";
}

async function refresh(): Promise<void> {
  // The browser only populates `label` after the user has granted
  // permission for at least one input device. Calling `getUserMedia`
  // with empty constraints first would force a prompt — too
  // intrusive here. So before granting: labels are blank, and we
  // render a "Grant access to see device names" hint inside each
  // picker. The deviceId column still works for selection.
  devices.value = await enumerateCallDevices();
}

const { engine } = useCallEngine();

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

let unsubscribe: (() => void) | null = null;

watch(open, async (isOpen) => {
  if (!isOpen) return;
  speakerSupported.value = detectSpeakerSupport();
  await refresh();
});

onMounted(() => {
  // `devicechange` fires when a device is plugged in/out. Re-
  // enumerate so the picker reflects current hardware.
  if (typeof navigator !== "undefined" && navigator.mediaDevices?.addEventListener) {
    const handler = () => {
      void refresh();
    };
    navigator.mediaDevices.addEventListener("devicechange", handler);
    unsubscribe = () => {
      navigator.mediaDevices.removeEventListener("devicechange", handler);
    };
  }
});

onUnmounted(() => {
  unsubscribe?.();
  unsubscribe = null;
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
</style>
