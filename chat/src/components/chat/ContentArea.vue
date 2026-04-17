<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Hash, MessageCircle, Settings, Search, X, Upload } from "lucide-vue-next";
import { extractImageFromEvent } from "@/lib/xmpp/file-upload";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { TimelineMessage, MarkupSpan } from "@/lib/chat-ui";
import type { XmppStatusSnapshot, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import { formatStamp } from "@/composables/useMessaging";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import UserPopover from "@/components/chat/UserPopover.vue";

const draft = defineModel<string>("draft", { required: true });

const props = defineProps<{
  waddle: WaddleSummary | null;
  channel: ChannelSummary | null;
  dmPeer?: { peerJid: string; peerUsername: string; presenceShow?: string } | null;
  sidebarMode?: "channels" | "dms";
  messages: TimelineMessage[];
  xmppStatus: XmppStatusSnapshot;
  actionError: string;
  isLoadingMessages: boolean;
  isSending: boolean;
  canManageChannels: boolean;
  typingUsers: string[];
  currentUser?: string;
  selfDomain?: string;
  avatarUrlByAuthor: Record<string, string | null>;
  authorJidByNick?: Record<string, string>;
  tenorApiKey: string;
  memberNames: string[];
  roomHats: RoomHats;
  roomPresence: RoomPresence;
  roomLastSeen: Record<string, number>;
  slowModeCooldown: number;
  searchResults: { id: string; nick: string; body: string; createdAt: string }[];
  isSearching: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
}>();

const emit = defineEmits<{
  send: [body: string, markup: MarkupSpan[]];
  typing: [];
  selectGif: [url: string];
  fileUpload: [file: File | Blob];
  editMessage: [messageId: string, newBody: string, markup?: MarkupSpan[]];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  displayed: [messageId: string];
  editChannel: [];
  search: [query: string];
  clearSearch: [];
  openDm: [peerJid: string];
}>();

const messagesContainer = ref<HTMLDivElement | null>(null);
const setMessagesContainer = (el: HTMLDivElement | null) => {
  messagesContainer.value = el;
};
const showSearch = ref(false);
const searchInput = ref("");
const avatarUrlByAuthor = computed(() => props.avatarUrlByAuthor ?? {});
const popoverAuthor = ref<{ username: string; jid: string } | null>(null);

defineExpose({ messagesContainer });

function doSearch() {
  emit("search", searchInput.value);
}

function closeSearch() {
  showSearch.value = false;
  searchInput.value = "";
  emit("clearSearch");
}

function presenceText(show?: string): string {
  if (show === "available") return "online";
  if (show === "away") return "away";
  if (show === "dnd") return "do not disturb";
  if (show === "xa") return "extended away";
  return "offline";
}

function onAvatarClick(author: string) {
  if (author === props.currentUser) return;
  const authorJid = props.authorJidByNick?.[author]
    ?? (props.selfDomain ? `${author}@${props.selfDomain}` : null);
  if (!authorJid) return;
  popoverAuthor.value = { username: author, jid: authorJid };
}

function closePopover() {
  popoverAuthor.value = null;
}

function openPopoverDm() {
  if (!popoverAuthor.value) return;
  emit("openDm", popoverAuthor.value.jid);
  popoverAuthor.value = null;
}

// -- Drag-and-drop file upload --
const isDragging = ref(false);
let dragLeaveTimeout: ReturnType<typeof setTimeout> | null = null;

function onDragEnter(e: DragEvent) {
  e.preventDefault();
  if (!e.dataTransfer?.types.includes("Files")) return;
  if (dragLeaveTimeout) { clearTimeout(dragLeaveTimeout); dragLeaveTimeout = null; }
  isDragging.value = true;
}

function onDragOver(e: DragEvent) {
  e.preventDefault();
}

function onDragLeave() {
  dragLeaveTimeout = setTimeout(() => { isDragging.value = false; }, 50);
}

function onDrop(e: DragEvent) {
  e.preventDefault();
  isDragging.value = false;
  const file = extractImageFromEvent(e);
  if (file) emit("fileUpload", file);
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
  <div
    class="flex-1 flex flex-col min-w-0 min-h-0 bg-background relative"
    @dragenter="onDragEnter"
    @dragover="onDragOver"
    @dragleave="onDragLeave"
    @drop="onDrop"
  >
    <!-- Drop zone overlay -->
    <div
      v-if="isDragging"
      class="absolute inset-0 z-50 bg-primary/10 border-2 border-dashed border-primary rounded-xl flex flex-col items-center justify-center pointer-events-none animate-fade-in"
    >
      <Upload class="w-8 h-8 text-primary mb-2" />
      <span class="text-primary font-display font-bold text-lg">Drop image to upload</span>
    </div>

    <!-- Header — sleek, floating feel -->
    <div class="h-14 border-b border-border px-6 flex items-center justify-between flex-shrink-0 glass-surface">
      <div class="flex items-center gap-3">
        <div class="flex items-center gap-2">
          <component :is="dmPeer ? MessageCircle : Hash" class="w-4 h-4 text-primary/70" />
          <h1 class="text-[15px] font-display font-bold tracking-tight">
            {{ dmPeer ? dmPeer.peerUsername : channel?.name ?? "..." }}
          </h1>
          <span v-if="dmPeer" class="text-[11px] text-muted-foreground">· {{ presenceText(dmPeer.presenceShow) }}</span>
        </div>
        <div
          v-if="xmppStatus.state !== 'online'"
          class="flex items-center gap-1.5 text-[11px] text-muted-foreground"
        >
          <span
            class="w-1.5 h-1.5 rounded-full inline-block"
            :class="{
              'bg-destructive': xmppStatus.state === 'error',
              'bg-warning animate-pulse': xmppStatus.state === 'reconnecting',
              'bg-muted-foreground': xmppStatus.state === 'offline',
            }"
          />
          {{ xmppStatus.state === 'reconnecting' ? 'reconnecting...' : xmppStatus.state }}
        </div>
      </div>
      <div class="flex gap-1">
        <button
          v-if="channel || dmPeer"
          class="h-8 w-8 flex items-center justify-center rounded-lg transition-all duration-200"
          :class="showSearch ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          title="Search messages"
          @click="showSearch = !showSearch"
        >
          <Search class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="canManageChannels && channel"
          class="h-8 w-8 flex items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground transition-all duration-200"
          title="Channel settings"
          @click="emit('editChannel')"
        >
          <Settings class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <!-- Search bar -->
    <div v-if="showSearch" class="px-6 py-2.5 border-b border-border glass-surface flex items-center gap-2.5 flex-shrink-0 animate-fade-in">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
      <input
        v-model="searchInput"
        placeholder="Search messages..."
        class="flex-1 text-[13px] bg-transparent focus:outline-none placeholder:text-muted-foreground/40"
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
    <div v-if="showSearch && (searchResults.length > 0 || isSearching)" class="border-b border-border glass-surface max-h-56 overflow-auto flex-shrink-0">
      <div v-if="isSearching" class="px-6 py-3 text-[13px] text-muted-foreground">
        Searching...
      </div>
      <div v-else class="divide-y divide-border">
        <div
          v-for="result in searchResults"
          :key="result.id"
          class="px-6 py-3 hover:bg-muted/50 transition-colors cursor-pointer"
        >
          <div class="flex items-baseline gap-2">
            <span class="font-medium text-[12px]">{{ result.nick }}</span>
            <span class="text-[11px] font-mono text-muted-foreground tabular-nums">{{ formatStamp(result.createdAt) }}</span>
          </div>
          <p class="text-[12px] text-muted-foreground truncate mt-0.5">{{ result.body }}</p>
        </div>
      </div>
    </div>

    <!-- Error banner -->
    <div
      v-if="actionError"
      class="px-6 py-2.5 bg-destructive/10 border-b border-destructive/20 text-[13px] text-destructive animate-fade-in"
    >
      <div>{{ actionError }}</div>
    </div>

    <!-- Messages -->
    <div :ref="setMessagesContainer" class="flex-1 min-h-0 overflow-auto px-6 py-4">
      <div v-if="isLoadingMessages" class="text-center py-16 text-[13px] text-muted-foreground">
        <div class="flex items-center justify-center gap-1.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </div>
        <p class="mt-3 text-muted-foreground/60">Loading messages...</p>
      </div>

      <div v-else-if="!channel && !dmPeer" class="flex flex-col items-center justify-center py-20">
        <div class="w-12 h-12 rounded-xl bg-muted flex items-center justify-center mb-4">
          <component :is="sidebarMode === 'dms' ? MessageCircle : Hash" class="w-5 h-5 text-primary/50" />
        </div>
        <p class="text-[14px] text-muted-foreground font-display">
          {{ sidebarMode === "dms" ? "Select a conversation" : "Select a channel to start chatting" }}
        </p>
      </div>

      <div v-else-if="messages.length === 0" class="flex flex-col items-center justify-center py-20">
        <div class="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center mb-4">
          <component :is="dmPeer ? MessageCircle : Hash" class="w-5 h-5 text-primary" />
        </div>
        <p class="text-[16px] font-display font-bold mb-1">
          {{ dmPeer ? `Conversation with @${dmPeer.peerUsername}` : `Welcome to #${channel?.name}` }}
        </p>
        <p class="text-[13px] text-muted-foreground">This is the start of the conversation.</p>
      </div>

      <div v-else class="max-w-none">
        <MessageCard
          v-for="msg in messages"
          :key="msg.id"
          :message="msg"
          :current-user="props.currentUser"
          :avatar-url="avatarUrlByAuthor[msg.author] ?? null"
          :hats="roomHats[msg.author] ?? []"
          :presence="roomPresence[msg.author] ?? 'offline'"
          :last-seen="roomLastSeen[msg.author]"
          :author-jid="authorJidByNick?.[msg.author]"
          @edit="(id, body, m) => emit('editMessage', id, body, m)"
          @retract="(id) => emit('retractMessage', id)"
          @react="(id, emoji) => emit('reactMessage', id, emoji)"
          @avatar-click="onAvatarClick"
        />
      </div>
    </div>

    <!-- Typing indicator -->
    <div
      v-if="typingUsers.length > 0"
      class="px-6 py-1.5 text-[11px] text-muted-foreground flex items-center gap-2 flex-shrink-0"
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
      v-if="channel || dmPeer"
      v-model:draft="draft"
      :channel-name="dmPeer ? dmPeer.peerUsername : (channel?.name ?? 'conversation')"
      :is-sending="isSending"
      :disabled="!channel && !dmPeer"
      :tenor-api-key="tenorApiKey"
      :member-names="memberNames"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      @send="(body, markup) => emit('send', body, markup)"
      @typing="emit('typing')"
      @select-gif="(url) => emit('selectGif', url)"
      @file-upload="(file) => emit('fileUpload', file)"
    />
    <UserPopover
      :open="!!popoverAuthor"
      :username="popoverAuthor?.username ?? ''"
      :avatar-url="popoverAuthor ? avatarUrlByAuthor[popoverAuthor.username] ?? null : null"
      :presence-text="dmPeer?.peerUsername === popoverAuthor?.username ? presenceText(dmPeer?.presenceShow) : undefined"
      :can-message="!!popoverAuthor"
      @close="closePopover"
      @message="openPopoverDm"
    />
  </div>
</template>
