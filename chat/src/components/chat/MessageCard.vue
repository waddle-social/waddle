<script setup lang="ts">
import { ref, computed } from "vue";
import { Check, CheckCheck, Pencil, SmilePlus, Trash2, Phone, Video, FileDown } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { renderStyledBody, isImageUrl, type TimelineMessage } from "@/lib/chat-ui";
import type { OccupantHat } from "@/lib/xmpp-client";
import { formatStamp } from "@/composables/useMessaging";

const HAT_LABELS: Record<string, string> = {
  "urn:xmpp:hats:owner": "OWNER",
  "urn:xmpp:hats:admin": "ADMIN",
  "urn:xmpp:hats:moderator": "MOD",
  "urn:xmpp:hats:bot": "BOT",
  "urn:xmpp:hats:verified": "VERIFIED",
};

const HAT_COLORS: Record<string, string> = {
  "urn:xmpp:hats:owner": "bg-warning/10 text-warning border-warning/20",
  "urn:xmpp:hats:admin": "bg-primary/10 text-primary border-primary/20",
  "urn:xmpp:hats:moderator": "bg-primary/10 text-primary border-primary/20",
  "urn:xmpp:hats:bot": "bg-success/10 text-success border-success/20",
  "urn:xmpp:hats:verified": "bg-primary/10 text-primary border-primary/20",
};

const props = defineProps<{
  message: TimelineMessage;
  currentUser?: string;
  hats: OccupantHat[];
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string];
  retract: [messageId: string];
  react: [messageId: string, emoji: string];
  joinCall: [invite: NonNullable<TimelineMessage["callInvite"]>];
}>();

const quickEmojis = ["👍", "❤️", "😂", "🎉", "👀"];

const styledHtml = computed(() => renderStyledBody(props.message.body));
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
const editDraft = ref("");

function startEdit() {
  editDraft.value = props.message.body;
  isEditing.value = true;
}

function cancelEdit() {
  isEditing.value = false;
  editDraft.value = "";
}

function submitEdit() {
  const trimmed = editDraft.value.trim();
  if (trimmed && trimmed !== props.message.body) {
    emit("edit", props.message.id, trimmed);
  }
  isEditing.value = false;
  editDraft.value = "";
}

function onEditKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    submitEdit();
  } else if (e.key === "Escape") {
    cancelEdit();
  }
}
</script>

<template>
  <!-- Retracted tombstone -->
  <div
    v-if="message.isRetracted"
    class="flex gap-2.5 px-3 py-1.5 opacity-40 animate-message-in"
  >
    <AppAvatar :name="message.author" size="md" />
    <div class="flex-1 min-w-0 pt-0.5">
      <div class="flex items-baseline gap-2 mb-0.5">
        <span class="font-medium text-[13px]">{{ message.author }}</span>
        <span class="text-[11px] font-mono text-muted-foreground">
          {{ formatStamp(message.createdAt) }}
        </span>
      </div>
      <p class="text-[13px] italic text-muted-foreground">This message was deleted.</p>
    </div>
  </div>

  <!-- Normal message -->
  <div
    v-else
    class="group flex gap-2.5 px-3 py-1.5 rounded-md transition-colors animate-message-in"
    :class="[
      isMentioned ? 'bg-warning/5 border-l-2 border-warning/30' : 'hover:bg-muted/50',
    ]"
  >
    <AppAvatar :name="message.author" size="md" class="mt-0.5" />
    <div class="flex-1 min-w-0">
      <div class="flex items-baseline gap-2 mb-0.5 flex-wrap">
        <span class="font-semibold text-[13px]">{{ message.author }}</span>
        <span
          v-for="hat in hats"
          :key="hat.uri"
          class="inline-block px-1.5 py-px text-[10px] font-medium uppercase tracking-wider rounded border leading-none"
          :class="HAT_COLORS[hat.uri] ?? 'bg-muted text-muted-foreground border-border'"
          :title="hat.title"
        >{{ HAT_LABELS[hat.uri] ?? hat.title }}</span>
        <span class="text-[11px] font-mono text-muted-foreground">
          {{ formatStamp(message.createdAt) }}
        </span>
        <span v-if="message.isEdited" class="text-[11px] text-muted-foreground">(edited)</span>
        <span
          v-if="message.isSelf && message.deliveryStatus"
          class="text-[11px] text-muted-foreground flex items-center gap-0.5"
        >
          <Check v-if="message.deliveryStatus === 'sending'" class="w-3 h-3" />
          <CheckCheck v-else-if="message.deliveryStatus === 'delivered'" class="w-3 h-3 text-primary" />
        </span>
        <span
          v-if="message.isSelf && message.readBy && message.readBy.length > 0"
          class="text-[11px] text-muted-foreground"
          :title="message.readBy.join(', ')"
        >
          Read by {{ message.readBy.length }}
        </span>
        <!-- Action toolbar -->
        <span v-if="message.isSelf && !isEditing" class="opacity-0 group-hover:opacity-100 transition-opacity flex items-center gap-0.5 ml-auto">
          <button
            class="p-1 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
            title="Edit message"
            @click="startEdit"
          >
            <Pencil class="w-3 h-3" />
          </button>
          <button
            class="p-1 rounded hover:bg-destructive/10 text-muted-foreground hover:text-destructive transition-colors"
            title="Delete message"
            @click="emit('retract', message.id)"
          >
            <Trash2 class="w-3 h-3" />
          </button>
        </span>
      </div>

      <!-- Edit mode -->
      <div v-if="isEditing" class="flex gap-2 mt-1">
        <input
          v-model="editDraft"
          class="flex-1 border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring px-3 bg-background text-[13px] h-8"
          @keydown="onEditKeydown"
        />
        <button
          class="text-[12px] font-medium px-2.5 h-8 rounded-md bg-primary text-primary-foreground hover:opacity-90 transition-colors"
          @click="submitEdit"
        >Save</button>
        <button
          class="text-[12px] font-medium px-2.5 h-8 rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
          @click="cancelEdit"
        >Cancel</button>
      </div>

      <!-- Call invite card -->
      <div v-else-if="message.callInvite" class="mt-1.5">
        <div class="inline-flex items-center gap-2.5 bg-primary/5 border border-primary/15 rounded-md p-2.5">
          <div class="flex items-center justify-center w-8 h-8 bg-primary text-primary-foreground rounded-md">
            <Video v-if="message.callInvite.muji || message.callInvite.jingleSid || message.callInvite.externalUri" class="w-3.5 h-3.5" />
            <Phone v-else class="w-3.5 h-3.5" />
          </div>
          <div class="flex-1 min-w-0">
            <div class="text-[13px] font-medium">
              {{ message.callInvite.meetingDesc ?? "Call Invite" }}
            </div>
            <div v-if="message.callInvite.externalUri" class="text-[11px] text-muted-foreground truncate">
              {{ message.callInvite.externalUri }}
            </div>
          </div>
          <button
            class="px-2.5 py-1 text-[12px] font-medium rounded-md bg-primary text-primary-foreground hover:opacity-90 transition-colors"
            @click="emit('joinCall', message.callInvite!)"
          >Join</button>
        </div>
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
      <div v-else-if="isSharedImage && message.sharedFile" class="mt-1.5">
        <a :href="message.sharedFile.url" target="_blank" rel="noopener noreferrer">
          <img
            :src="message.sharedFile.url"
            :alt="message.sharedFile.name ?? 'Shared image'"
            class="max-w-xs max-h-56 rounded-md border border-border object-contain"
            loading="lazy"
          />
        </a>
        <div v-if="message.sharedFile.name" class="text-[11px] text-muted-foreground mt-1">
          {{ message.sharedFile.name }}
          <span v-if="message.sharedFile.size"> · {{ formatFileSize(message.sharedFile.size) }}</span>
        </div>
      </div>

      <!-- Shared file (non-image) -->
      <div v-else-if="message.sharedFile" class="mt-1.5">
        <a
          :href="message.sharedFile.url"
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-2.5 bg-muted rounded-md p-2.5 hover:bg-muted/80 transition-colors border border-border"
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
      <div v-else-if="isGif" class="mt-1.5">
        <img
          :src="message.body.trim()"
          alt="GIF"
          class="max-w-xs max-h-56 rounded-md border border-border object-contain"
          loading="lazy"
        />
      </div>

      <!-- Normal display -->
      <div v-else class="text-[13px] leading-relaxed break-words styled-body" v-html="styledHtml" />

      <!-- Reactions -->
      <div v-if="message.reactions && Object.keys(message.reactions).length > 0" class="flex flex-wrap gap-1 mt-1.5">
        <button
          v-for="(nicks, emoji) in message.reactions"
          :key="emoji"
          class="inline-flex items-center gap-1 px-1.5 py-0.5 text-[12px] rounded-md border border-border bg-muted/50 hover:bg-muted transition-colors"
          :title="nicks.join(', ')"
          @click="emit('react', message.id, emoji)"
        >
          <span>{{ emoji }}</span>
          <span class="text-muted-foreground font-mono text-[10px]">{{ nicks.length }}</span>
        </button>
        <button
          class="opacity-0 group-hover:opacity-100 inline-flex items-center px-1.5 py-0.5 text-[12px] text-muted-foreground hover:text-foreground rounded-md hover:bg-muted transition-all"
          title="Add reaction"
          @click.stop
        >
          <span class="relative">
            <SmilePlus class="w-3 h-3" />
            <span class="absolute left-full top-1/2 -translate-y-1/2 ml-1 flex gap-0.5 whitespace-nowrap">
              <button
                v-for="e in quickEmojis"
                :key="e"
                class="hover:scale-110 transition-transform"
                @click="emit('react', message.id, e)"
              >{{ e }}</button>
            </span>
          </span>
        </button>
      </div>

      <!-- Quick react (no reactions yet) -->
      <div v-else class="opacity-0 group-hover:opacity-100 flex gap-0.5 mt-1 transition-opacity">
        <button
          v-for="e in quickEmojis"
          :key="e"
          class="text-[13px] hover:scale-110 transition-transform p-0.5"
          :title="`React with ${e}`"
          @click="emit('react', message.id, e)"
        >{{ e }}</button>
      </div>
    </div>
  </div>
</template>
