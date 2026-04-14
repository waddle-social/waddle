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
  // XEP-0513: Broadcast mentions highlight for everyone
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
    class="flex gap-4 border border-foreground/30 p-4 opacity-50"
  >
    <AppAvatar :name="message.author" size="md" />
    <div class="flex-1 min-w-0">
      <div class="flex items-baseline gap-3 mb-1">
        <span class="font-mono font-bold text-sm">{{ message.author }}</span>
        <span class="text-xs font-mono text-muted-foreground">
          {{ formatStamp(message.createdAt) }}
        </span>
      </div>
      <p class="text-sm italic text-muted-foreground">This message was deleted.</p>
    </div>
  </div>

  <!-- Normal message -->
  <div
    v-else
    class="group flex gap-4 border p-4 transition-colors"
    :class="[
      message.isSelf ? 'bg-muted/30 border-foreground' : 'border-foreground',
      isMentioned ? 'border-l-4 border-l-yellow-500 bg-yellow-500/5' : '',
    ]"
  >
    <AppAvatar :name="message.author" size="md" />
    <div class="flex-1 min-w-0">
      <div class="flex items-baseline gap-3 mb-1">
        <span class="font-mono font-bold text-sm">{{ message.author }}</span>
        <span
          v-for="hat in hats"
          :key="hat.uri"
          class="inline-block px-1 py-0.5 text-[10px] font-mono font-bold uppercase tracking-wider border border-foreground/40 text-muted-foreground leading-none"
          :title="hat.title"
        >{{ HAT_LABELS[hat.uri] ?? hat.title }}</span>
        <span class="text-xs font-mono text-muted-foreground">
          {{ formatStamp(message.createdAt) }}
        </span>
        <span v-if="message.isEdited" class="text-xs font-mono text-muted-foreground">(edited)</span>
        <span
          v-if="message.isSelf && message.deliveryStatus"
          class="text-xs font-mono text-muted-foreground flex items-center gap-1"
        >
          <Check v-if="message.deliveryStatus === 'sending'" class="w-3 h-3" />
          <CheckCheck v-else-if="message.deliveryStatus === 'delivered'" class="w-3 h-3" />
        </span>
        <span
          v-if="message.isSelf && message.readBy && message.readBy.length > 0"
          class="text-xs font-mono text-muted-foreground"
          :title="message.readBy.join(', ')"
        >
          Read by {{ message.readBy.length }}
        </span>
        <span v-if="message.isSelf && !isEditing" class="opacity-0 group-hover:opacity-100 transition-opacity flex items-center gap-1">
          <button
            class="text-muted-foreground hover:text-foreground"
            title="Edit message"
            @click="startEdit"
          >
            <Pencil class="w-3 h-3" />
          </button>
          <button
            class="text-muted-foreground hover:text-destructive"
            title="Delete message"
            @click="emit('retract', message.id)"
          >
            <Trash2 class="w-3 h-3" />
          </button>
        </span>
      </div>

      <!-- Edit mode -->
      <div v-if="isEditing" class="flex gap-2">
        <input
          v-model="editDraft"
          class="flex-1 font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground px-2 bg-background text-sm h-8"
          @keydown="onEditKeydown"
        />
        <button
          class="text-xs font-mono px-2 h-8 border border-foreground hover:bg-foreground hover:text-background transition-colors"
          @click="submitEdit"
        >Save</button>
        <button
          class="text-xs font-mono px-2 h-8 text-muted-foreground hover:text-foreground transition-colors"
          @click="cancelEdit"
        >Cancel</button>
      </div>

      <!-- XEP-0482/0483: Call invite card -->
      <div v-else-if="message.callInvite" class="mt-1">
        <div class="inline-flex items-center gap-3 border border-foreground p-3">
          <div class="flex items-center justify-center w-8 h-8 bg-foreground text-background">
            <Video v-if="message.callInvite.muji || message.callInvite.jingleSid || message.callInvite.externalUri" class="w-4 h-4" />
            <Phone v-else class="w-4 h-4" />
          </div>
          <div class="flex-1 min-w-0">
            <div class="font-mono text-sm font-bold">
              {{ message.callInvite.meetingDesc ?? "Call Invite" }}
            </div>
            <div v-if="message.callInvite.externalUri" class="text-xs font-mono text-muted-foreground truncate">
              {{ message.callInvite.externalUri }}
            </div>
          </div>
          <button
            class="px-3 py-1.5 text-xs font-mono font-bold uppercase tracking-wider border border-foreground hover:bg-foreground hover:text-background transition-colors"
            @click="emit('joinCall', message.callInvite!)"
          >Join</button>
        </div>
      </div>

      <!-- XEP-0449: Sticker (large image, no metadata) -->
      <div v-else-if="message.isSticker && message.sharedFile" class="mt-1">
        <img
          :src="message.sharedFile.url"
          :alt="message.sharedFile.desc ?? message.body ?? 'Sticker'"
          class="max-w-32 max-h-32 object-contain"
          loading="lazy"
        />
      </div>

      <!-- XEP-0447: Shared file (image inline) -->
      <div v-else-if="isSharedImage && message.sharedFile" class="mt-1">
        <a :href="message.sharedFile.url" target="_blank" rel="noopener noreferrer">
          <img
            :src="message.sharedFile.url"
            :alt="message.sharedFile.name ?? 'Shared image'"
            class="max-w-xs max-h-64 border border-foreground/20 object-contain"
            loading="lazy"
          />
        </a>
        <div v-if="message.sharedFile.name" class="text-xs font-mono text-muted-foreground mt-0.5">
          {{ message.sharedFile.name }}
          <span v-if="message.sharedFile.size"> · {{ formatFileSize(message.sharedFile.size) }}</span>
        </div>
      </div>

      <!-- XEP-0447: Shared file (non-image download card) -->
      <div v-else-if="message.sharedFile" class="mt-1">
        <a
          :href="message.sharedFile.url"
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-3 border border-foreground p-3 hover:bg-muted transition-colors"
        >
          <FileDown class="w-5 h-5 flex-shrink-0" />
          <div class="flex-1 min-w-0">
            <div class="font-mono text-sm font-bold truncate">{{ message.sharedFile.name ?? "File" }}</div>
            <div class="text-xs font-mono text-muted-foreground">
              {{ message.sharedFile.mediaType ?? "file" }}
              <span v-if="message.sharedFile.size"> · {{ formatFileSize(message.sharedFile.size) }}</span>
            </div>
          </div>
        </a>
      </div>

      <!-- Inline image/GIF -->
      <div v-else-if="isGif" class="mt-1">
        <img
          :src="message.body.trim()"
          alt="GIF"
          class="max-w-xs max-h-64 border border-foreground/20 object-contain"
          loading="lazy"
        />
      </div>

      <!-- Normal display (XEP-0393 styled) -->
      <div v-else class="text-sm leading-relaxed break-words styled-body" v-html="styledHtml" />

      <!-- Reactions display -->
      <div v-if="message.reactions && Object.keys(message.reactions).length > 0" class="flex flex-wrap gap-1 mt-2">
        <button
          v-for="(nicks, emoji) in message.reactions"
          :key="emoji"
          class="inline-flex items-center gap-1 px-1.5 py-0.5 text-xs border border-foreground/30 hover:border-foreground transition-colors font-mono"
          :title="nicks.join(', ')"
          @click="emit('react', message.id, emoji)"
        >
          <span>{{ emoji }}</span>
          <span class="text-muted-foreground">{{ nicks.length }}</span>
        </button>
        <button
          class="opacity-0 group-hover:opacity-100 inline-flex items-center px-1.5 py-0.5 text-xs text-muted-foreground hover:text-foreground border border-transparent hover:border-foreground/30 transition-all"
          title="Add reaction"
          @click.stop
        >
          <span class="relative">
            <SmilePlus class="w-3.5 h-3.5" />
            <span class="absolute left-full top-1/2 -translate-y-1/2 ml-1 flex gap-0.5 whitespace-nowrap">
              <button
                v-for="e in quickEmojis"
                :key="e"
                class="hover:scale-125 transition-transform"
                @click="emit('react', message.id, e)"
              >{{ e }}</button>
            </span>
          </span>
        </button>
      </div>

      <!-- Quick react (when no reactions yet) -->
      <div v-else class="opacity-0 group-hover:opacity-100 flex gap-1 mt-1 transition-opacity">
        <button
          v-for="e in quickEmojis"
          :key="e"
          class="text-sm hover:scale-125 transition-transform"
          :title="`React with ${e}`"
          @click="emit('react', message.id, e)"
        >{{ e }}</button>
      </div>
    </div>
  </div>
</template>
