<script setup lang="ts">
import { ref, watch, onUnmounted, computed } from "vue";
import { Mic, MicOff, Video, VideoOff, PhoneOff, Minimize2, Maximize2 } from "lucide-vue-next";

const props = defineProps<{
  localStream: MediaStream | null;
  remoteParticipants: Map<string, { jid: string; stream: MediaStream }>;
  micEnabled: boolean;
  cameraEnabled: boolean;
  phase: string;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCamera: [];
  endCall: [];
}>();

const collapsed = ref(false);
const localVideoRef = ref<HTMLVideoElement | null>(null);
const remoteVideoRefs = ref<Map<string, HTMLVideoElement>>(new Map());

const participantCount = computed(() => props.remoteParticipants.size);

const gridCols = computed(() => {
  const count = props.remoteParticipants.size;
  if (count === 0) return "grid-cols-1";
  if (count <= 3) return "grid-cols-2";
  return "grid-cols-3";
});

watch(
  () => props.localStream,
  (stream) => {
    if (localVideoRef.value) {
      localVideoRef.value.srcObject = stream;
    }
  },
);

function setRemoteVideoRef(key: string, el: HTMLVideoElement | null) {
  if (el) {
    remoteVideoRefs.value.set(key, el);
    const participant = props.remoteParticipants.get(key);
    if (participant) {
      el.srcObject = participant.stream;
    }
  } else {
    remoteVideoRefs.value.delete(key);
  }
}

watch(
  () => props.remoteParticipants,
  (participants) => {
    for (const [key, el] of remoteVideoRefs.value) {
      const participant = participants.get(key);
      if (participant) {
        el.srcObject = participant.stream;
      }
    }
  },
  { deep: true },
);

onUnmounted(() => {
  if (localVideoRef.value) {
    localVideoRef.value.srcObject = null;
  }
  for (const el of remoteVideoRefs.value.values()) {
    el.srcObject = null;
  }
});
</script>

<template>
  <div class="border-b border-border bg-background">
    <!-- Collapsed mode -->
    <div
      v-if="collapsed"
      class="px-6 h-11 flex items-center justify-between glass-surface"
    >
      <span class="text-xs text-muted-foreground flex items-center gap-2">
        <span class="w-2 h-2 rounded-full bg-success shadow-[0_0_6px_var(--success)] animate-pulse" />
        Call · {{ participantCount }} participant{{ participantCount !== 1 ? "s" : "" }} · {{ phase }}
      </span>
      <div class="flex items-center gap-1">
        <button
          class="h-8 w-8 flex items-center justify-center rounded-lg hover:bg-muted transition-all duration-200"
          title="Expand call"
          @click="collapsed = false"
        >
          <Maximize2 class="w-3.5 h-3.5" />
        </button>
        <button
          class="h-8 w-8 flex items-center justify-center rounded-lg text-destructive hover:bg-destructive/10 transition-all duration-200"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <!-- Expanded mode -->
    <div v-else class="flex flex-col">
      <!-- Video grid -->
      <div class="relative bg-muted/20 min-h-48">
        <div
          v-if="remoteParticipants.size === 0"
          class="flex items-center justify-center h-48"
        >
          <div class="text-center">
            <div class="flex items-center justify-center gap-1.5 mb-2">
              <span class="typing-dot" />
              <span class="typing-dot" />
              <span class="typing-dot" />
            </div>
            <span class="text-sm text-muted-foreground">Waiting for others to join...</span>
          </div>
        </div>

        <div
          v-else
          class="grid gap-1.5 p-1.5"
          :class="gridCols"
        >
          <div
            v-for="[key, participant] in remoteParticipants"
            :key="key"
            class="relative bg-muted aspect-video rounded-lg overflow-hidden"
          >
            <video
              :ref="(el) => setRemoteVideoRef(key, el as HTMLVideoElement | null)"
              autoplay
              playsinline
              class="w-full h-full object-cover"
            />
            <div class="absolute bottom-1.5 left-1.5 px-2 py-0.5 bg-background/60 backdrop-blur-sm text-xs rounded-md">
              {{ participant.jid.split("@")[0] || "Participant" }}
            </div>
          </div>
        </div>

        <!-- Local preview (picture-in-picture) -->
        <div class="absolute bottom-3 right-3 w-32 aspect-video rounded-lg border border-border bg-muted overflow-hidden shadow-lg">
          <video
            ref="localVideoRef"
            autoplay
            playsinline
            muted
            class="w-full h-full object-cover"
            style="transform: scaleX(-1)"
          />
        </div>
      </div>

      <!-- Controls bar -->
      <div class="px-6 py-3 flex items-center justify-center gap-2.5 border-t border-border glass-surface">
        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl transition-all duration-200"
          :class="micEnabled
            ? 'bg-muted hover:bg-muted/80'
            : 'bg-destructive/10 text-destructive hover:bg-destructive/20'"
          :title="micEnabled ? 'Mute microphone' : 'Unmute microphone'"
          @click="emit('toggleMic')"
        >
          <Mic v-if="micEnabled" class="w-4 h-4" />
          <MicOff v-else class="w-4 h-4" />
        </button>

        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl transition-all duration-200"
          :class="cameraEnabled
            ? 'bg-muted hover:bg-muted/80'
            : 'bg-destructive/10 text-destructive hover:bg-destructive/20'"
          :title="cameraEnabled ? 'Disable camera' : 'Enable camera'"
          @click="emit('toggleCamera')"
        >
          <Video v-if="cameraEnabled" class="w-4 h-4" />
          <VideoOff v-else class="w-4 h-4" />
        </button>

        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl bg-destructive text-white hover:shadow-[0_0_16px_rgba(239,68,68,0.4)] transition-all duration-200"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-4 h-4" />
        </button>

        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl bg-muted hover:bg-muted/80 transition-all duration-200"
          title="Minimize call"
          @click="collapsed = true"
        >
          <Minimize2 class="w-4 h-4" />
        </button>
      </div>
    </div>
  </div>
</template>
