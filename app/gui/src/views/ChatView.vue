<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue';
import { storeToRefs } from 'pinia';
import { useRoute } from 'vue-router';

import { useRouter } from 'vue-router';
import { type ChatMessage, type UnlistenFn, useWaddle } from '../composables/useWaddle';
import EmbedCard from '../components/EmbedCard.vue';
import AvatarImage from '../components/AvatarImage.vue';
import { useRuntimeStore, type MessageDeliveryStatus } from '../stores/runtime';
import { useRoomsStore } from '../stores/rooms';
import { useAuthStore } from '../stores/auth';
import { useVCardStore } from '../stores/vcard';
import { extractMessageFromEventPayload } from '../utils/eventPayload';
import { decodeRouteParam } from '../utils/routeParams';

const route = useRoute();
const router = useRouter();
const { getHistory, listen, sendMessage } = useWaddle();
const runtimeStore = useRuntimeStore();
const roomsStore = useRoomsStore();
const authStore = useAuthStore();
const vcardStore = useVCardStore();
const { connectionStatus } = storeToRefs(runtimeStore);
const { joinedRooms } = storeToRefs(roomsStore);

const jid = computed(() => decodeRouteParam(String(route.params.jid ?? '')));

const isRoom = computed(() => joinedRooms.value.has(jid.value));

const displayName = computed(() => {
  return vcardStore.getDisplayName(jid.value);
});

const displayAvatar = computed(() => {
  return vcardStore.getAvatarUrl(jid.value);
});

const messages = ref<ChatMessage[]>([]);
const draft = ref('');
const loading = ref(false);
const sending = ref(false);
const error = ref<string | null>(null);
const messagesContainer = ref<HTMLElement | null>(null);

function sortMessages(items: ChatMessage[]): ChatMessage[] {
  return [...items].sort((left, right) => {
    const leftTime = Date.parse(left.timestamp);
    const rightTime = Date.parse(right.timestamp);
    if (Number.isNaN(leftTime) || Number.isNaN(rightTime)) {
      return left.id.localeCompare(right.id);
    }
    return leftTime - rightTime;
  });
}

function formatTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function formatDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return '';
  return date.toLocaleDateString([], { weekday: 'long', month: 'long', day: 'numeric', year: 'numeric' });
}

function bareJid(value: string): string {
  return value.split('/')[0] || value;
}

function isOutbound(message: ChatMessage): boolean {
  if (isRoom.value) {
    // MUC: outbound if the nick in from (room@conf/nick) matches our nick.
    // Our nick is the localpart of our JID (used by the rooms store when joining).
    const nick = message.from.split('/')[1];
    return !!nick && nick === authStore.nickname;
  }
  const toBare = bareJid(message.to);
  return toBare === jid.value;
}

function senderName(message: ChatMessage): string {
  if (isOutbound(message)) return 'You';
  const from = message.from || jid.value;
  // For MUC, the resource part is the nick (from is room@conf/nick)
  if (isRoom.value) {
    const resource = from.split('/')[1];
    if (resource) return resource;
  }
  return vcardStore.getDisplayName(bareJid(from));
}

function senderAvatar(message: ChatMessage): string | null {
  if (isOutbound(message)) return vcardStore.ownAvatarUrl;
  const from = message.from || jid.value;
  return vcardStore.getAvatarUrl(bareJid(from));
}

/**
 * Resolve a MUC sender's nick to a bare JID for DM.
 * In semi-anonymous MUC rooms the real JID isn't exposed, so we infer
 * nick@domain using the logged-in user's server domain.
 */
function senderBareJid(message: ChatMessage): string | null {
  if (!isRoom.value) return null;
  if (isOutbound(message)) return null;
  const nick = message.from.split('/')[1];
  if (!nick) return null;
  const domain = authStore.jid.split('@')[1];
  if (!domain) return null;
  return `${nick}@${domain}`;
}

function openDmWithSender(message: ChatMessage): void {
  const targetJid = senderBareJid(message);
  if (!targetJid) return;
  void router.push(`/chat/${encodeURIComponent(targetJid)}`);
}

function getAvatarColor(name: string): string {
  const colors = [
    '#5865f2', '#57f287', '#fee75c', '#eb459e',
    '#ed4245', '#3ba55c', '#faa61a', '#e67e22',
  ];
  let hash = 0;
  for (const ch of name) {
    hash = ch.charCodeAt(0) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length] ?? colors[0] ?? '#5865f2';
}

function getInitials(name: string): string {
  return name.slice(0, 2).toUpperCase();
}

function defaultOutboundStatus(): MessageDeliveryStatus {
  return connectionStatus.value === 'connected' ? 'sent' : 'queued';
}

function deliveryStatusFor(message: ChatMessage): MessageDeliveryStatus | null {
  if (!isOutbound(message)) return null;
  // No delivery receipts in MUC
  if (isRoom.value) return null;
  return runtimeStore.deliveryFor(message.id) ?? 'sent';
}

function deliveryIcon(status: MessageDeliveryStatus | null): string {
  switch (status) {
    case 'queued': return '◷';
    case 'sent': return '✓';
    case 'delivered': return '✓✓';
    default: return '';
  }
}

function showHeader(index: number): boolean {
  if (index === 0) return true;
  const prev = messages.value[index - 1];
  const curr = messages.value[index];
  if (!prev || !curr) return true;
  if (senderName(prev) !== senderName(curr)) return true;
  const gap = Date.parse(curr.timestamp) - Date.parse(prev.timestamp);
  return gap > 5 * 60 * 1000;
}

function showDateSeparator(index: number): boolean {
  if (index === 0) return true;
  const prevMessage = messages.value[index - 1];
  const currMessage = messages.value[index];
  if (!prevMessage || !currMessage) return true;
  const prev = new Date(prevMessage.timestamp).toDateString();
  const curr = new Date(currMessage.timestamp).toDateString();
  return prev !== curr;
}

function seedOutboundStatuses(history: ChatMessage[]): void {
  if (isRoom.value) return;
  for (const message of history) {
    if (!isOutbound(message) || runtimeStore.deliveryFor(message.id)) continue;
    runtimeStore.setMessageDelivery(message.id, 'sent');
  }
}

function scrollToBottom(): void {
  void nextTick(() => {
    if (messagesContainer.value) {
      messagesContainer.value.scrollTop = messagesContainer.value.scrollHeight;
    }
  });
}

async function loadHistory(): Promise<void> {
  if (!jid.value) {
    messages.value = [];
    return;
  }
  loading.value = true;
  error.value = null;
  try {
    const history = await getHistory(jid.value, 100);
    messages.value = sortMessages(history);
    seedOutboundStatuses(messages.value);
    scrollToBottom();
  } catch (cause) {
    messages.value = [];
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}

async function submitMessage(): Promise<void> {
  const body = draft.value.trim();
  if (!jid.value || !body || sending.value) return;

  sending.value = true;
  error.value = null;
  draft.value = '';

  try {
    const msgType = isRoom.value ? 'groupchat' : 'chat';
    const sent = await sendMessage(jid.value, body, msgType);
    if (!isRoom.value) {
      runtimeStore.setMessageDelivery(sent.id, defaultOutboundStatus());
    }
    messages.value = sortMessages([...messages.value, sent]);
    scrollToBottom();
  } catch (cause) {
    draft.value = body;
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    sending.value = false;
  }
}

function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    void submitMessage();
  }
}

function maybeRefreshForEvent(event: unknown): void {
  const message = extractMessageFromEventPayload<ChatMessage>(event);
  if (!message) return;

  const fromBare = bareJid(message.from);
  const toBare = bareJid(message.to);

  if (fromBare === jid.value || toBare === jid.value) {
    const alreadyPresent = messages.value.some((existing) => existing.id === message.id);
    if (!alreadyPresent) {
      messages.value = sortMessages([...messages.value, message]);
      scrollToBottom();
    }
  }
}

let stopMessageListener: UnlistenFn | null = null;
let historyReloadTimer: ReturnType<typeof setTimeout> | null = null;

onMounted(async () => {
  await runtimeStore.bootstrap();

  // Set up the message listener FIRST so we don't miss messages
  stopMessageListener = await listen<unknown>('xmpp.message.received', ({ payload }) => {
    maybeRefreshForEvent(payload);
  });

  // Now load history (catches anything already in the transport's buffer)
  await loadHistory();

  // For MUC rooms, schedule a re-load after a short delay to catch
  // history messages that may still be arriving from the server.
  if (isRoom.value) {
    historyReloadTimer = setTimeout(() => { void loadHistory(); }, 1500);
  }
});

onUnmounted(() => {
  stopMessageListener?.();
  stopMessageListener = null;
  if (historyReloadTimer) {
    clearTimeout(historyReloadTimer);
    historyReloadTimer = null;
  }
});

watch(jid, () => {
  // Clear the reload timer when switching conversations
  if (historyReloadTimer) {
    clearTimeout(historyReloadTimer);
    historyReloadTimer = null;
  }
  void loadHistory();
  // Re-schedule reload for MUC rooms
  if (isRoom.value) {
    historyReloadTimer = setTimeout(() => { void loadHistory(); }, 1500);
  }
});
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Channel header bar -->
    <header class="flex h-12 flex-shrink-0 items-center border-b border-border px-4 shadow-sm">
      <span class="mr-2 text-xl text-muted">{{ isRoom ? '#' : '@' }}</span>
      <h2 class="text-base font-semibold text-foreground">{{ displayName }}</h2>
      <span class="ml-3 hidden text-sm text-muted sm:inline">{{ jid }}</span>
      <span v-if="isRoom" class="ml-2 rounded bg-surface-raised px-2 py-0.5 text-[10px] text-muted">Room</span>
    </header>

    <!-- Error banner -->
    <div v-if="error" class="mx-4 mt-2 rounded bg-danger/20 px-3 py-2 text-sm text-danger">
      {{ error }}
    </div>

    <!-- Messages area -->
    <div ref="messagesContainer" class="flex-1 overflow-y-auto px-4 pb-4">
      <p v-if="loading" class="py-8 text-center text-sm text-muted">Loading messages…</p>

      <div v-else-if="messages.length === 0" class="flex flex-col items-center justify-center py-16">
        <AvatarImage
          :jid="jid"
          :name="displayName"
          :photo-url="displayAvatar"
          :size="64"
        />
        <h3 class="text-xl font-semibold text-foreground">{{ displayName }}</h3>
        <p class="mt-1 text-sm text-muted">
          <template v-if="isRoom">
            This is the beginning of <strong>#{{ displayName }}</strong>.
          </template>
          <template v-else>
            This is the beginning of your direct message history with <strong>{{ displayName }}</strong>.
          </template>
        </p>
      </div>

      <template v-else>
        <template v-for="(message, index) in messages" :key="message.id">
          <!-- Date separator -->
          <div v-if="showDateSeparator(index)" class="my-4 flex items-center gap-2">
            <div class="h-px flex-1 bg-border"></div>
            <span class="text-xs font-semibold text-muted">{{ formatDate(message.timestamp) }}</span>
            <div class="h-px flex-1 bg-border"></div>
          </div>

          <!-- Message row -->
          <div
            class="group relative rounded px-2 py-0.5 transition-colors hover:bg-hover"
            :class="showHeader(index) ? 'mt-4' : 'mt-0'"
          >
            <!-- Full message with avatar -->
            <div v-if="showHeader(index)" class="flex gap-4">
              <div
                :class="senderBareJid(message) ? 'cursor-pointer ring-0 transition-all hover:ring-2 hover:ring-accent rounded-full' : ''"
                :title="senderBareJid(message) ? `DM ${senderBareJid(message)}` : undefined"
                @click="openDmWithSender(message)"
              >
                <AvatarImage
                  :jid="bareJid(message.from)"
                  :name="senderName(message)"
                  :photo-url="senderAvatar(message)"
                  :size="40"
                />
              </div>
              <div class="min-w-0 flex-1">
                <div class="flex items-baseline gap-2">
                  <span
                    class="text-sm font-semibold"
                    :class="senderBareJid(message) ? 'cursor-pointer hover:underline' : ''"
                    :style="{ color: getAvatarColor(senderName(message)) }"
                    :title="senderBareJid(message) ? `DM ${senderBareJid(message)}` : undefined"
                    @click="openDmWithSender(message)"
                  >
                    {{ senderName(message) }}
                  </span>
                  <span class="text-[11px] text-muted">{{ formatTime(message.timestamp) }}</span>
                  <span v-if="deliveryStatusFor(message)" class="text-[11px] text-muted">
                    {{ deliveryIcon(deliveryStatusFor(message)) }}
                  </span>
                </div>
                <p class="whitespace-pre-wrap text-sm leading-relaxed text-foreground">{{ message.body }}</p>
                <EmbedCard
                  v-for="(embed, embedIdx) in (message.embeds ?? [])"
                  :key="`${message.id}-embed-${embedIdx}`"
                  :embed="embed"
                />
              </div>
            </div>

            <!-- Compact message (continuation) -->
            <div v-else class="flex gap-4">
              <div class="w-10 flex-shrink-0 pt-0.5 text-center">
                <span class="hidden text-[10px] text-muted group-hover:inline">
                  {{ formatTime(message.timestamp) }}
                </span>
              </div>
              <div class="min-w-0 flex-1">
                <p class="whitespace-pre-wrap text-sm leading-relaxed text-foreground">{{ message.body }}</p>
                <EmbedCard
                  v-for="(embed, embedIdx) in (message.embeds ?? [])"
                  :key="`${message.id}-embed-c-${embedIdx}`"
                  :embed="embed"
                />
              </div>
            </div>
          </div>
        </template>
      </template>
    </div>

    <!-- Message input -->
    <div class="flex-shrink-0 px-4 pb-6">
      <form
        class="flex items-center rounded-lg bg-surface-raised"
        @submit.prevent="submitMessage"
      >
        <input
          v-model="draft"
          type="text"
          :placeholder="isRoom ? `Message #${displayName}` : `Message @${displayName}`"
          class="min-w-0 flex-1 bg-transparent px-4 py-3 text-sm text-foreground placeholder-muted outline-none"
          autocomplete="off"
          @keydown="handleKeydown"
        />
        <button
          v-if="draft.trim()"
          type="submit"
          class="mr-2 flex h-8 w-8 items-center justify-center rounded bg-accent text-white transition-colors hover:bg-accent/80 disabled:opacity-50"
          :disabled="sending || !jid"
        >
          <svg class="h-4 w-4" fill="currentColor" viewBox="0 0 20 20">
            <path d="M10.894 2.553a1 1 0 00-1.788 0l-7 14a1 1 0 001.169 1.409l5-1.429A1 1 0 009 15.571V11a1 1 0 112 0v4.571a1 1 0 00.725.962l5 1.428a1 1 0 001.17-1.408l-7-14z" />
          </svg>
        </button>
      </form>
    </div>
  </div>
</template>
