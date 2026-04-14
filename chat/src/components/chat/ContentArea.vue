<script setup lang="ts">
import { ref, watch } from "vue";
import { Hash, Settings, Search, X, Phone, Video, PhoneOff, Mic, MicOff, VideoOff, RefreshCw } from "lucide-vue-next";
import type { ActiveCallSummary, ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { MujiCallPhase, XmppStatusSnapshot, RoomHats } from "@/lib/xmpp-client";
import { formatStamp } from "@/composables/useMessaging";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";

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
  activeCalls: ActiveCallSummary[];
  isLoadingActiveCalls: boolean;
  joiningListedCallId: string | null;
  mujiInCall: boolean;
  mujiIsSwitching: boolean;
  mujiCurrentSid: string | null;
  mujiHasRemoteTracks: boolean;
  mujiMicEnabled: boolean;
  mujiCameraEnabled: boolean;
  mujiServiceJid: string | null;
  mujiError: string;
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
  refreshActiveCalls: [];
  joinListedCall: [callId: string, video: boolean];
  switchListedCall: [callId: string, video: boolean];
  dismissCallInvite: [];
  toggleMic: [];
  toggleCamera: [];
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

function formatCallTimestamp(value: string) {
  return formatStamp(value);
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
  <div class="flex-1 flex flex-col min-w-0">
    <!-- Header -->
    <div class="h-20 border-b border-foreground px-6 flex items-center justify-between bg-background flex-shrink-0">
      <div class="flex items-center gap-4">
        <div class="flex items-center gap-2">
          <Hash class="w-4 h-4" />
          <h1 class="text-xl font-mono font-bold">{{ channel?.name ?? "..." }}</h1>
        </div>
        <div
          v-if="xmppStatus.state !== 'online'"
          class="flex items-center gap-2 text-xs font-mono text-muted-foreground"
        >
          <span
            class="w-2 h-2 inline-block"
            :class="xmppStatus.state === 'error' ? 'bg-destructive' : 'bg-muted-foreground'"
          />
          {{ xmppStatus.state }}
        </div>
      </div>
      <div class="flex gap-1">
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
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          :class="showSearch ? 'bg-muted' : ''"
          title="Search messages"
          @click="showSearch = !showSearch"
        >
          <Search class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="canManageChannels && channel"
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
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

    <div
      v-if="channel && mujiInCall"
      class="px-6 py-2 border-b border-foreground bg-muted/20 text-xs font-mono text-muted-foreground"
    >
      <span class="font-bold text-foreground">Call:</span>
      {{ mujiPhase }}
      <span v-if="mujiServiceJid"> · {{ mujiServiceJid }}</span>
      <span v-if="mujiHasRemoteTracks"> · remote media connected</span>
    </div>

    <div
      v-if="channel"
      class="px-6 py-3 border-b border-foreground bg-muted/10 space-y-2"
    >
      <div class="flex items-center justify-between">
        <span class="text-xs font-mono font-bold uppercase tracking-wider">Active calls</span>
        <button
          class="h-6 px-2 text-[10px] font-mono uppercase tracking-wider border border-foreground/60 hover:bg-foreground hover:text-background transition-colors flex items-center gap-1"
          :disabled="isLoadingActiveCalls"
          @click="emit('refreshActiveCalls')"
        >
          <RefreshCw class="w-3 h-3" :class="isLoadingActiveCalls ? 'animate-spin' : ''" />
          Refresh
        </button>
      </div>

      <div v-if="isLoadingActiveCalls && activeCalls.length === 0" class="text-xs font-mono text-muted-foreground">
        Loading active calls...
      </div>

      <div v-else-if="activeCalls.length > 0" class="space-y-2">
        <div
          v-for="call in activeCalls"
          :key="call.call_id"
          class="border border-foreground/30 px-3 py-2 flex items-center justify-between gap-3"
        >
          <div class="min-w-0 space-y-1">
            <div class="text-xs font-mono">
              <span class="font-bold uppercase">{{ call.state }}</span>
              · {{ call.participant_count }} participant{{ call.participant_count === 1 ? "" : "s" }}
            </div>
            <div class="text-[10px] font-mono text-muted-foreground truncate">
              Created {{ formatCallTimestamp(call.created_at) }} · Updated {{ formatCallTimestamp(call.updated_at) }}
            </div>
          </div>
          <button
            class="px-3 py-1.5 text-xs font-mono font-bold uppercase tracking-wider border border-foreground hover:bg-foreground hover:text-background transition-colors disabled:opacity-60 disabled:cursor-not-allowed"
            :disabled="mujiIsSwitching || !!joiningListedCallId || (mujiInCall && mujiCurrentSid === call.call_id)"
            @click="mujiInCall ? emit('switchListedCall', call.call_id, true) : emit('joinListedCall', call.call_id, true)"
          >
            {{
              joiningListedCallId === call.call_id
                ? "Joining..."
                : mujiInCall
                  ? "Switch"
                  : "Join"
            }}
          </button>
        </div>
      </div>
      <div v-else class="text-xs font-mono text-muted-foreground">
        No active calls in this channel.
      </div>
    </div>

    <!-- XEP-0431: Search bar -->
    <div v-if="showSearch" class="px-6 py-2 border-b border-foreground bg-background flex items-center gap-2 flex-shrink-0">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
      <input
        v-model="searchInput"
        placeholder="Search messages..."
        class="flex-1 font-mono text-sm bg-transparent focus:outline-none"
        @keydown.enter="doSearch"
      />
      <button
        v-if="searchInput"
        class="text-muted-foreground hover:text-foreground"
        @click="closeSearch"
      >
        <X class="w-3.5 h-3.5" />
      </button>
    </div>

    <!-- Search results -->
    <div v-if="showSearch && (searchResults.length > 0 || isSearching)" class="border-b border-foreground bg-muted/30 max-h-64 overflow-auto flex-shrink-0">
      <div v-if="isSearching" class="px-6 py-4 text-sm font-mono text-muted-foreground">
        Searching...
      </div>
      <div v-else class="divide-y divide-foreground/10">
        <div
          v-for="result in searchResults"
          :key="result.id"
          class="px-6 py-2 hover:bg-muted/50 transition-colors"
        >
          <div class="flex items-baseline gap-2">
            <span class="font-mono font-bold text-xs">{{ result.nick }}</span>
            <span class="text-[10px] font-mono text-muted-foreground">{{ formatStamp(result.createdAt) }}</span>
          </div>
          <p class="text-xs font-mono text-muted-foreground truncate">{{ result.body }}</p>
        </div>
      </div>
    </div>

    <!-- Error banner -->
    <div
      v-if="actionError || mujiError"
      class="px-6 py-3 bg-destructive/10 border-b border-destructive/20 text-sm font-mono text-destructive"
    >
      {{ actionError || mujiError }}
    </div>

    <!-- Messages -->
    <div ref="messagesContainer" class="flex-1 overflow-auto px-6 py-6">
      <div v-if="isLoadingMessages" class="text-center py-8 text-sm font-mono text-muted-foreground">
        Loading messages...
      </div>

      <div v-else-if="!channel" class="text-center py-8 text-sm font-mono text-muted-foreground">
        Select a channel to start chatting
      </div>

      <div v-else-if="messages.length === 0" class="text-center py-8 text-sm font-mono text-muted-foreground">
        No messages yet. Start the conversation!
      </div>

      <div v-else class="space-y-4 max-w-4xl">
        <MessageCard
          v-for="msg in messages"
          :key="msg.id"
          :message="msg"
          :current-user="props.currentUser"
          :hats="roomHats[msg.author] ?? []"
          @edit="(id, body) => emit('editMessage', id, body)"
          @retract="(id) => emit('retractMessage', id)"
          @react="(id, emoji) => emit('reactMessage', id, emoji)"
        />
      </div>
    </div>

    <!-- Typing indicator -->
    <div
      v-if="typingUsers.length > 0"
      class="px-6 py-1.5 text-xs font-mono text-muted-foreground flex-shrink-0"
    >
      <span v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing...</span>
      <span v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing...</span>
      <span v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing...</span>
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
