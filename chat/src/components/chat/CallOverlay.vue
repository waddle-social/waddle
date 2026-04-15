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
  <div class="border-b border-foreground bg-background">
    <!-- Collapsed mode -->
    <div
      v-if="collapsed"
      class="px-6 h-10 flex items-center justify-between"
    >
      <span class="text-xs font-mono text-muted-foreground">
        Call · {{ participantCount }} participant{{ participantCount !== 1 ? "s" : "" }} · {{ phase }}
      </span>
      <div class="flex items-center gap-1">
        <button
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          title="Expand call"
          @click="collapsed = false"
        >
          <Maximize2 class="w-3.5 h-3.5" />
        </button>
        <button
          class="h-7 w-7 flex items-center justify-center text-destructive hover:bg-destructive/10 transition-colors"
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
      <div class="relative bg-muted/30 min-h-48">
        <div
          v-if="remoteParticipants.size === 0"
          class="flex items-center justify-center h-48"
        >
          <span class="text-sm font-mono text-muted-foreground">Waiting for others to join...</span>
        </div>

        <div
          v-else
          class="grid gap-1 p-1"
          :class="gridCols"
        >
          <div
            v-for="[key, participant] in remoteParticipants"
            :key="key"
            class="relative bg-muted aspect-video"
          >
            <video
              :ref="(el) => setRemoteVideoRef(key, el as HTMLVideoElement | null)"
              autoplay
              playsinline
              class="w-full h-full object-cover"
            />
            <div class="absolute bottom-1 left-1 px-1.5 py-0.5 bg-background/70 text-xs font-mono">
              {{ participant.jid.split("@")[0] || "Participant" }}
            </div>
          </div>
        </div>

        <!-- Local preview (picture-in-picture) -->
        <div class="absolute bottom-2 right-2 w-32 aspect-video border border-foreground bg-muted overflow-hidden">
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
      <div class="px-6 py-3 flex items-center justify-center gap-3 border-t border-foreground/20">
        <button
          class="h-9 w-9 flex items-center justify-center border transition-colors"
          :class="micEnabled
            ? 'border-foreground hover:bg-muted'
            : 'border-destructive bg-destructive/10 text-destructive hover:bg-destructive/20'"
          :title="micEnabled ? 'Mute microphone' : 'Unmute microphone'"
          @click="emit('toggleMic')"
        >
          <Mic v-if="micEnabled" class="w-3.5 h-3.5" />
          <MicOff v-else class="w-3.5 h-3.5" />
        </button>

        <button
          class="h-9 w-9 flex items-center justify-center border transition-colors"
          :class="cameraEnabled
            ? 'border-foreground hover:bg-muted'
            : 'border-destructive bg-destructive/10 text-destructive hover:bg-destructive/20'"
          :title="cameraEnabled ? 'Disable camera' : 'Enable camera'"
          @click="emit('toggleCamera')"
        >
          <Video v-if="cameraEnabled" class="w-3.5 h-3.5" />
          <VideoOff v-else class="w-3.5 h-3.5" />
        </button>

        <button
          class="h-9 w-9 flex items-center justify-center bg-destructive text-white border border-destructive hover:bg-destructive/90 transition-colors"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-3.5 h-3.5" />
        </button>

        <button
          class="h-9 w-9 flex items-center justify-center border border-foreground/40 hover:bg-muted transition-colors"
          title="Minimize call"
          @click="collapsed = true"
        >
          <Minimize2 class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  </div>
</template>
