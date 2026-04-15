<script setup lang="ts">
import { ref, computed } from "vue";
import { Check, CheckCheck, Pencil, SmilePlus, Trash2, Phone, Video, FileDown } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import { renderStyledBody, isImageUrl, type TimelineMessage, type MarkupSpan } from "@/lib/chat-ui";
import { serializeTiptapToXep0393 } from "@/lib/editor/xep0393-serializer";
import { parseXep0393ToTiptap } from "@/lib/editor/xep0393-parser";
import type { OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatStamp } from "@/composables/useMessaging";

const HAT_LABELS: Record<string, string> = {
  "urn:xmpp:hats:owner": "OWNER",
  "urn:xmpp:hats:admin": "ADMIN",
  "urn:xmpp:hats:moderator": "MOD",
  "urn:xmpp:hats:bot": "BOT",
  "urn:xmpp:hats:verified": "VERIFIED",
};

const HAT_COLORS: Record<string, string> = {
  "urn:xmpp:hats:owner": "bg-warning/10 text-warning",
  "urn:xmpp:hats:admin": "bg-primary/10 text-primary",
  "urn:xmpp:hats:moderator": "bg-primary/10 text-primary",
  "urn:xmpp:hats:bot": "bg-success/10 text-success",
  "urn:xmpp:hats:verified": "bg-primary/10 text-primary",
};

const props = defineProps<{
  message: TimelineMessage;
  currentUser?: string;
  hats: OccupantHat[];
  avatarUrl?: string | null;
  presence?: OccupantPresence;
  lastSeen?: number;
  authorJid?: string;
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string, markup?: MarkupSpan[]];
  retract: [messageId: string];
  react: [messageId: string, emoji: string];
  joinCall: [invite: NonNullable<TimelineMessage["callInvite"]>];
  avatarClick: [author: string];
}>();

const quickEmojis = ["👍", "❤️", "😂", "🎉", "👀"];

const isSystemEvent = computed(() => !!props.message.callInvite);

const styledHtml = computed(() => renderStyledBody(props.message.body, props.message.markup));
const isGif = computed(() => isImageUrl(props.message.body));
const isSharedImage = computed(() => {
  const f = props.message.sharedFile;
  return f && f.disposition === "inline" && f.mediaType?.startsWith("image/");
});

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const isMentioned = computed(() => {
  if (props.message.broadcastMention) return true;
  if (!props.currentUser || !props.message.mentions) return false;
  return props.message.mentions.some(
    (m) => m === props.currentUser || m.split("@")[0] === props.currentUser,
  );
});

const isEditing = ref(false);
const editInitialContent = ref<Record<string, unknown> | undefined>(undefined);
const editEditorRef = ref<InstanceType<typeof ChatEditor> | null>(null);

function startEdit() {
  editInitialContent.value = parseXep0393ToTiptap(props.message.body);
  isEditing.value = true;
}

function cancelEdit() {
  isEditing.value = false;
  editInitialContent.value = undefined;
}

function submitEditFromEditor(doc: Record<string, unknown>) {
  const { body, markup } = serializeTiptapToXep0393(doc);
  const trimmed = body.trim();
  if (trimmed && trimmed !== props.message.body) {
    emit("edit", props.message.id, trimmed, markup);
  }
  isEditing.value = false;
  editInitialContent.value = undefined;
}

function emitAvatarClick() {
  if (props.authorJid && !props.message.isSelf) {
    emit("avatarClick", props.message.author);
  }
}
</script>

<template>
  <!-- Retracted tombstone -->
  <div
    v-if="message.isRetracted"
    class="flex gap-3 px-3 py-2 opacity-30 animate-message-in"
  >
    <AppAvatar :name="message.author" :src="avatarUrl" :presence="presence" :last-seen="lastSeen" size="md" />
    <div class="flex-1 min-w-0 pt-0.5">
      <div class="flex items-baseline gap-2 mb-0.5">
        <span class="font-medium text-[13px]">{{ message.author }}</span>
        <span class="text-[11px] font-mono text-muted-foreground tabular-nums">
          {{ formatStamp(message.createdAt) }}
        </span>
      </div>
      <p class="text-[13px] italic text-muted-foreground">This message was deleted.</p>
    </div>
  </div>

  <!-- System event (call invites, etc.) -->
  <div
    v-else-if="isSystemEvent"
    class="flex items-center justify-center gap-3 py-3 animate-message-in"
  >
    <div class="h-px flex-1 bg-border" />
    <div class="flex items-center gap-2.5 px-3 py-1.5 rounded-full bg-muted/40 text-[12px] text-muted-foreground">
      <Phone v-if="message.callInvite && !message.callInvite.jingleSid && !message.callInvite.externalUri" class="w-3 h-3 text-primary/60" />
      <Video v-else class="w-3.5 h-3.5 text-primary/60" />
      <span>
        <span class="font-medium text-foreground/70">{{ message.author }}</span>
        · {{ message.callInvite?.meetingDesc ?? message.body }}
      </span>
      <span class="text-[10px] font-mono text-muted-foreground/50 tabular-nums">{{ formatStamp(message.createdAt) }}</span>
      <button
        v-if="message.callInvite"
        class="px-2 py-0.5 text-[11px] font-semibold rounded-md bg-primary/10 text-primary hover:bg-primary/20 transition-all duration-200"
        @click="emit('joinCall', message.callInvite!)"
      >Join</button>
    </div>
    <div class="h-px flex-1 bg-border" />
  </div>

  <!-- Normal message -->
  <div
    v-else
    class="group flex gap-3 px-3 py-2 rounded-xl transition-all duration-200 animate-message-in"
    :class="[
      isMentioned ? 'bg-warning/5 border-l-2 border-warning/30' : 'hover:bg-muted/40',
    ]"
  >
    <button
      class="mt-0.5 rounded-lg"
      type="button"
      @click.stop="emitAvatarClick"
    >
      <AppAvatar :name="message.author" :src="avatarUrl" :presence="presence" :last-seen="lastSeen" size="md" />
    </button>
    <div class="flex-1 min-w-0">
      <div class="flex items-baseline gap-2 mb-0.5 flex-wrap">
        <span class="font-semibold text-[13px]">{{ message.author }}</span>
        <span
          v-for="hat in hats"
          :key="hat.uri"
          class="inline-block px-1.5 py-px text-[9px] font-bold uppercase tracking-wider rounded-md leading-none"
          :class="HAT_COLORS[hat.uri] ?? 'bg-muted text-muted-foreground'"
          :title="hat.title"
        >{{ HAT_LABELS[hat.uri] ?? hat.title }}</span>
        <span class="text-[11px] font-mono text-muted-foreground/60 tabular-nums">
          {{ formatStamp(message.createdAt) }}
        </span>
        <span v-if="message.isEdited" class="text-[11px] text-muted-foreground/50">(edited)</span>
        <span
          v-if="message.isSelf && message.deliveryStatus"
          class="text-[11px] text-muted-foreground flex items-center gap-0.5"
        >
          <Check v-if="message.deliveryStatus === 'sending'" class="w-3 h-3" />
          <CheckCheck v-else-if="message.deliveryStatus === 'delivered'" class="w-3 h-3 text-primary" />
        </span>
        <span
          v-if="message.isSelf && message.readBy && message.readBy.length > 0"
          class="text-[11px] text-muted-foreground/50"
          :title="message.readBy.join(', ')"
        >
          Read by {{ message.readBy.length }}
        </span>
        <!-- Action toolbar -->
        <span v-if="message.isSelf && !isEditing" class="opacity-0 group-hover:opacity-100 transition-opacity flex items-center gap-0.5 ml-auto">
          <button
            class="p-1.5 rounded-lg hover:bg-muted text-muted-foreground hover:text-foreground transition-all duration-200"
            title="Edit message"
            @click="startEdit"
          >
            <Pencil class="w-3 h-3" />
          </button>
          <button
            class="p-1.5 rounded-lg hover:bg-destructive/10 text-muted-foreground hover:text-destructive transition-all duration-200"
            title="Delete message"
            @click="emit('retract', message.id)"
          >
            <Trash2 class="w-3 h-3" />
          </button>
        </span>
      </div>

      <!-- Edit mode -->
      <div v-if="isEditing" class="flex gap-2 mt-1 items-end">
        <ChatEditor
          ref="editEditorRef"
          compact
          :initial-content="editInitialContent"
          placeholder="Edit message..."
          @send="submitEditFromEditor"
          @cancel="cancelEdit"
          class="flex-1"
        />
        <button
          class="text-[12px] font-medium px-3 h-9 rounded-xl text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-200 shrink-0"
          @click="cancelEdit"
        >Cancel</button>
      </div>

      <!-- Sticker -->
      <div v-else-if="message.isSticker && message.sharedFile" class="mt-1">
        <img
          :src="message.sharedFile.url"
          :alt="message.sharedFile.desc ?? message.body ?? 'Sticker'"
          class="max-w-28 max-h-28 object-contain"
          loading="lazy"
        />
      </div>

      <!-- Shared file (image inline) -->
      <div v-else-if="isSharedImage && message.sharedFile" class="mt-2">
        <a :href="message.sharedFile.url" target="_blank" rel="noopener noreferrer">
          <img
            :src="message.sharedFile.url"
            :alt="message.sharedFile.name ?? 'Shared image'"
            class="max-w-xs max-h-56 rounded-xl border border-border object-contain"
            loading="lazy"
          />
        </a>
        <div v-if="message.sharedFile.name" class="text-[11px] text-muted-foreground/60 mt-1">
          {{ message.sharedFile.name }}
          <span v-if="message.sharedFile.size"> · {{ formatFileSize(message.sharedFile.size) }}</span>
        </div>
      </div>

      <!-- Shared file (non-image) -->
      <div v-else-if="message.sharedFile" class="mt-2">
        <a
          :href="message.sharedFile.url"
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-3 bg-muted rounded-xl p-3 hover:bg-muted/80 transition-all duration-200"
        >
          <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
          <div class="flex-1 min-w-0">
            <div class="text-[13px] font-medium truncate">{{ message.sharedFile.name ?? "File" }}</div>
            <div class="text-[11px] text-muted-foreground">
              {{ message.sharedFile.mediaType ?? "file" }}
              <span v-if="message.sharedFile.size"> · {{ formatFileSize(message.sharedFile.size) }}</span>
            </div>
          </div>
        </a>
      </div>

      <!-- Inline GIF -->
      <div v-else-if="isGif" class="mt-2">
        <img
          :src="message.body.trim()"
          alt="GIF"
          class="max-w-xs max-h-56 rounded-xl border border-border object-contain"
          loading="lazy"
        />
      </div>

      <!-- Normal display -->
      <div v-else class="text-[13px] leading-relaxed break-words styled-body" v-html="styledHtml" />

      <!-- Reactions -->
      <div v-if="message.reactions && Object.keys(message.reactions).length > 0" class="flex flex-wrap gap-1 mt-2">
        <button
          v-for="(nicks, emoji) in message.reactions"
          :key="emoji"
          class="inline-flex items-center gap-1 px-2 py-0.5 text-[12px] rounded-lg bg-muted/60 hover:bg-muted transition-all duration-200"
          :title="nicks.join(', ')"
          @click="emit('react', message.id, emoji)"
        >
          <span>{{ emoji }}</span>
          <span class="text-muted-foreground font-mono text-[10px] tabular-nums">{{ nicks.length }}</span>
        </button>
        <button
          class="opacity-0 group-hover:opacity-100 inline-flex items-center px-2 py-0.5 text-[12px] text-muted-foreground hover:text-foreground rounded-lg hover:bg-muted transition-all duration-200"
          title="Add reaction"
          @click.stop
        >
          <span class="relative">
            <SmilePlus class="w-3 h-3" />
            <span class="absolute left-full top-1/2 -translate-y-1/2 ml-1 flex gap-0.5 whitespace-nowrap">
              <button
                v-for="e in quickEmojis"
                :key="e"
                class="hover:scale-125 transition-transform duration-150"
                @click="emit('react', message.id, e)"
              >{{ e }}</button>
            </span>
          </span>
        </button>
      </div>

      <!-- Quick react (no reactions yet) -->
      <div v-else class="opacity-0 group-hover:opacity-100 flex gap-0.5 mt-1.5 transition-opacity">
        <button
          v-for="e in quickEmojis"
          :key="e"
          class="text-[13px] hover:scale-125 transition-transform duration-150 p-0.5"
          :title="`React with ${e}`"
          @click="emit('react', message.id, e)"
        >{{ e }}</button>
      </div>
    </div>
  </div>
</template>
