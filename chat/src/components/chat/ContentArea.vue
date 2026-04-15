<script setup lang="ts">
import { ref, watch } from "vue";
import { Hash, Settings, Search, X, Phone, Video, PhoneOff, Mic, MicOff, VideoOff } from "lucide-vue-next";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { MujiCallPhase, XmppStatusSnapshot, RoomHats } from "@/lib/xmpp-client";
import { formatStamp } from "@/composables/useMessaging";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import CallOverlay from "@/components/chat/CallOverlay.vue";

const draft = defineModel<string>("draft", { required: true });

const props = defineProps<{
  waddle: WaddleSummary | null;
  channel: ChannelSummary | null;
  messages: TimelineMessage[];
  xmppStatus: XmppStatusSnapshot;
  actionError: string;
  isLoadingMessages: boolean;
  isSending: boolean;
  canManageChannels: boolean;
  typingUsers: string[];
  currentUser?: string;
  tenorApiKey: string;
  memberNames: string[];
  roomHats: RoomHats;
  slowModeCooldown: number;
  searchResults: { id: string; nick: string; body: string; createdAt: string }[];
  isSearching: boolean;
  mujiPhase: MujiCallPhase;
  mujiPendingInvite: { fromNick: string; invite: { jingleJid?: string; meetingDesc?: string } } | null;
  mujiInCall: boolean;
  mujiIsSwitching: boolean;
  mujiCurrentSid: string | null;
  mujiHasRemoteTracks: boolean;
  mujiMicEnabled: boolean;
  mujiCameraEnabled: boolean;
  mujiServiceJid: string | null;
  mujiError: string;
  mujiLocalStream: MediaStream | null;
  mujiRemoteParticipants: Map<string, { jid: string; stream: MediaStream }>;
}>();

const emit = defineEmits<{
  send: [];
  typing: [];
  selectGif: [url: string];
  editMessage: [messageId: string, newBody: string];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  displayed: [messageId: string];
  editChannel: [];
  search: [query: string];
  clearSearch: [];
  startAudioCall: [];
  startVideoCall: [];
  endCall: [];
  joinCallInvite: [video: boolean];
  switchCallInvite: [video: boolean];
  declineCallInvite: [];
  dismissCallInvite: [];
  toggleMic: [];
  toggleCamera: [];
  joinCallFromMessage: [invite: { inviteId: string; muji: boolean; jingleSid?: string; jingleJid?: string; externalUri?: string; meetingDesc?: string }];
}>();

const messagesContainer = ref<HTMLDivElement | null>(null);
const showSearch = ref(false);
const searchInput = ref("");

defineExpose({ messagesContainer });

function doSearch() {
  emit("search", searchInput.value);
}

function closeSearch() {
  showSearch.value = false;
  searchInput.value = "";
  emit("clearSearch");
}

// XEP-0333: Send displayed marker for the latest non-self message
watch(
  () => props.messages,
  (msgs) => {
    const last = [...msgs].reverse().find((m) => !m.isSelf && !m.isRetracted);
    if (last) {
      emit("displayed", last.id);
    }
  },
  { deep: true },
);
</script>

<template>
  <div class="flex-1 flex flex-col min-w-0 min-h-0 bg-background">
    <!-- Header — thin, precise -->
    <div class="h-12 border-b border-border px-5 flex items-center justify-between flex-shrink-0">
      <div class="flex items-center gap-2.5">
        <div class="flex items-center gap-1.5">
          <Hash class="w-4 h-4 text-muted-foreground" />
          <h1 class="text-[14px] font-semibold">{{ channel?.name ?? "..." }}</h1>
        </div>
        <div
          v-if="xmppStatus.state !== 'online'"
          class="flex items-center gap-1.5 text-[11px] text-muted-foreground"
        >
          <span
            class="w-1.5 h-1.5 rounded-full inline-block"
            :class="xmppStatus.state === 'error' ? 'bg-destructive' : 'bg-warning animate-pulse'"
          />
          {{ xmppStatus.state }}
        </div>
      </div>
      <div class="flex gap-0.5">
        <button
          v-if="channel && !mujiInCall"
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          title="Start audio call"
          @click="emit('startAudioCall')"
        >
          <Phone class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel && !mujiInCall"
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          title="Start video call"
          @click="emit('startVideoCall')"
        >
          <Video class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel && mujiInCall"
          class="h-7 w-7 flex items-center justify-center text-destructive hover:bg-destructive/10 transition-colors"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel && mujiInCall"
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          :title="mujiMicEnabled ? 'Mute microphone' : 'Unmute microphone'"
          @click="emit('toggleMic')"
        >
          <Mic v-if="mujiMicEnabled" class="w-3.5 h-3.5" />
          <MicOff v-else class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel && mujiInCall"
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          :title="mujiCameraEnabled ? 'Disable camera' : 'Enable camera'"
          @click="emit('toggleCamera')"
        >
          <Video v-if="mujiCameraEnabled" class="w-3.5 h-3.5" />
          <VideoOff v-else class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel"
          class="h-7 w-7 flex items-center justify-center rounded-md transition-colors"
          :class="showSearch ? 'bg-muted text-foreground' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          title="Search messages"
          @click="showSearch = !showSearch"
        >
          <Search class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="canManageChannels && channel"
          class="h-7 w-7 flex items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
          title="Channel settings"
          @click="emit('editChannel')"
        >
          <Settings class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <div
      v-if="mujiPendingInvite"
      class="px-6 py-3 border-b border-foreground bg-muted/30 flex items-center justify-between gap-4"
    >
      <div class="min-w-0">
        <div class="text-sm font-mono font-bold">
          {{ mujiPendingInvite.fromNick }} invited you to a call
          <span v-if="mujiInCall"> while you're already in a call</span>
        </div>
        <div class="text-xs font-mono text-muted-foreground truncate">
          {{ mujiPendingInvite.invite.meetingDesc ?? mujiPendingInvite.invite.jingleJid ?? "Muji call invite" }}
        </div>
      </div>
      <div class="flex items-center gap-2">
        <button
          class="px-3 py-1.5 text-xs font-mono font-bold uppercase tracking-wider border border-foreground hover:bg-foreground hover:text-background transition-colors"
          @click="mujiInCall ? emit('switchCallInvite', true) : emit('joinCallInvite', true)"
        >
          {{ mujiInCall ? "Switch" : "Join" }}
        </button>
        <button
          class="px-3 py-1.5 text-xs font-mono font-bold uppercase tracking-wider border border-foreground/60 text-muted-foreground hover:text-foreground transition-colors"
          @click="emit('declineCallInvite')"
        >
          Decline
        </button>
        <button
          class="text-muted-foreground hover:text-foreground"
          title="Dismiss invite"
          @click="emit('dismissCallInvite')"
        >
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <CallOverlay
      v-if="mujiInCall"
      :local-stream="mujiLocalStream"
      :remote-participants="mujiRemoteParticipants"
      :mic-enabled="mujiMicEnabled"
      :camera-enabled="mujiCameraEnabled"
      :phase="mujiPhase"
      @toggle-mic="emit('toggleMic')"
      @toggle-camera="emit('toggleCamera')"
      @end-call="emit('endCall')"
    />

    <!-- Search bar -->
    <div v-if="showSearch" class="px-5 py-2 border-b border-border bg-surface flex items-center gap-2 flex-shrink-0 animate-fade-in">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
      <input
        v-model="searchInput"
        placeholder="Search messages..."
        class="flex-1 text-[13px] bg-transparent focus:outline-none placeholder:text-muted-foreground/50"
        @keydown.enter="doSearch"
      />
      <button
        v-if="searchInput"
        class="p-0.5 rounded text-muted-foreground hover:text-foreground transition-colors"
        @click="closeSearch"
      >
        <X class="w-3.5 h-3.5" />
      </button>
    </div>

    <!-- Search results -->
    <div v-if="showSearch && (searchResults.length > 0 || isSearching)" class="border-b border-border bg-surface max-h-56 overflow-auto flex-shrink-0">
      <div v-if="isSearching" class="px-5 py-3 text-[13px] text-muted-foreground">
        Searching...
      </div>
      <div v-else class="divide-y divide-border">
        <div
          v-for="result in searchResults"
          :key="result.id"
          class="px-5 py-2.5 hover:bg-muted/50 transition-colors cursor-pointer"
        >
          <div class="flex items-baseline gap-2">
            <span class="font-medium text-[12px]">{{ result.nick }}</span>
            <span class="text-[11px] font-mono text-muted-foreground">{{ formatStamp(result.createdAt) }}</span>
          </div>
          <p class="text-[12px] text-muted-foreground truncate mt-0.5">{{ result.body }}</p>
        </div>
      </div>
    </div>

    <!-- Error banner -->
    <div
      v-if="actionError || mujiError"
      class="px-5 py-2.5 bg-destructive/10 border-b border-destructive/20 text-[13px] text-destructive animate-fade-in"
    >
      <div v-if="actionError">{{ actionError }}</div>
      <div v-if="mujiError && mujiError !== actionError">{{ mujiError }}</div>
    </div>

    <!-- Messages -->
    <div ref="messagesContainer" class="flex-1 min-h-0 overflow-auto px-5 py-3">
      <div v-if="isLoadingMessages" class="text-center py-12 text-[13px] text-muted-foreground">
        <div class="flex items-center justify-center gap-1.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </div>
        <p class="mt-2">Loading messages...</p>
      </div>

      <div v-else-if="!channel" class="flex flex-col items-center justify-center py-16">
        <div class="w-10 h-10 rounded-lg bg-muted flex items-center justify-center mb-3">
          <Hash class="w-5 h-5 text-muted-foreground" />
        </div>
        <p class="text-[13px] text-muted-foreground">Select a channel to start chatting</p>
      </div>

      <div v-else-if="messages.length === 0" class="flex flex-col items-center justify-center py-16">
        <div class="w-10 h-10 rounded-lg bg-muted flex items-center justify-center mb-3">
          <Hash class="w-5 h-5 text-muted-foreground" />
        </div>
        <p class="text-[14px] font-medium mb-1">Welcome to #{{ channel.name }}</p>
        <p class="text-[13px] text-muted-foreground">This is the start of the conversation.</p>
      </div>

      <div v-else class="max-w-none">
        <MessageCard
          v-for="msg in messages"
          :key="msg.id"
          :message="msg"
          :current-user="props.currentUser"
          :hats="roomHats[msg.author] ?? []"
          @edit="(id, body) => emit('editMessage', id, body)"
          @retract="(id) => emit('retractMessage', id)"
          @react="(id, emoji) => emit('reactMessage', id, emoji)"
          @join-call="(invite) => emit('joinCallFromMessage', invite)"
        />
      </div>
    </div>

    <!-- Typing indicator -->
    <div
      v-if="typingUsers.length > 0"
      class="px-5 py-1 text-[11px] text-muted-foreground flex items-center gap-1.5 flex-shrink-0"
    >
      <span class="flex gap-0.5">
        <span class="typing-dot" />
        <span class="typing-dot" />
        <span class="typing-dot" />
      </span>
      <span v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</span>
      <span v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</span>
      <span v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</span>
    </div>

    <!-- Composer -->
    <MessageComposer
      v-if="channel"
      v-model:draft="draft"
      :channel-name="channel.name"
      :is-sending="isSending"
      :disabled="!channel"
      :tenor-api-key="tenorApiKey"
      :member-names="memberNames"
      :slow-mode-cooldown="slowModeCooldown"
      @send="emit('send')"
      @typing="emit('typing')"
      @select-gif="(url) => emit('selectGif', url)"
    />
  </div>
</template>
