<script setup lang="ts">
import { ref, computed, nextTick, onMounted, onUnmounted, watch } from "vue";
import { MoreHorizontal, Pencil, Reply, SmilePlus, Trash2, FileDown, CornerDownRight } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import ImageLightbox from "@/components/ui/ImageLightbox.vue";
import { renderStyledBody, isImageUrl, type TimelineMessage, type MarkupSpan } from "@/lib/chat-ui";
import { serializeTiptapToXep0393 } from "@/lib/editor/xep0393-serializer";
import { parseXep0393ToTiptap } from "@/lib/editor/xep0393-parser";
import { applyShikiToCodeBlocks } from "@/lib/shiki";
import type { OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatStamp } from "@/composables/useMessaging";
import { useLongPress } from "@/composables/useLongPress";

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
  reply: [message: TimelineMessage];
  scrollToMessage: [messageId: string];
  avatarClick: [author: string];
}>();

const quickEmojis = ["👍", "❤️", "😂", "🎉", "👀"];

const styledHtml = computed(() => renderStyledBody(displayBody.value, props.message.markup));
const styledBodyRef = ref<HTMLDivElement | null>(null);
const setStyledBodyRef = (el: HTMLDivElement | null) => {
  styledBodyRef.value = el;
};
const sharedFiles = computed(() => props.message.sharedFiles ?? []);
const isGif = computed(() => sharedFiles.value.length === 0 && isImageUrl(props.message.body));

const imageAttachments = computed(() =>
  sharedFiles.value.filter((f) => f.disposition === "inline" && f.mediaType?.startsWith("image/")),
);
const nonImageAttachments = computed(() =>
  sharedFiles.value.filter((f) => !(f.disposition === "inline" && f.mediaType?.startsWith("image/"))),
);
/** Display the user's text body when present; hide body when it's just the fallback URL for a single image. */
const displayBody = computed(() => {
  const body = props.message.body;
  if (!body) return "";
  if (sharedFiles.value.length === 0) return body;
  const matchesAttachment = sharedFiles.value.some((f) => f.url === body.trim());
  return matchesAttachment ? "" : body;
});

const lightboxOpen = ref(false);
const lightboxIndex = ref(0);
const lightboxImages = computed(() =>
  imageAttachments.value.map((f) => {
    const img: { url: string; name?: string; width?: number; height?: number } = { url: f.url };
    if (f.name) img.name = f.name;
    if (f.width) img.width = f.width;
    if (f.height) img.height = f.height;
    return img;
  }),
);

function openLightbox(index: number) {
  lightboxIndex.value = index;
  lightboxOpen.value = true;
}

async function highlightMessageCodeBlocks() {
  const el = styledBodyRef.value;
  if (!el) return;
  await applyShikiToCodeBlocks(el);
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const replyAuthorName = computed(() => {
  const author = props.message.replyTo?.author;
  if (!author) return "";
  const nickPart = author.includes("/") ? author.split("/").pop()! : author.split("@")[0];
  return nickPart ?? author;
});

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
const setEditEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editEditorRef.value = instance;
};

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

const bubbleEl = ref<HTMLElement | null>(null);
const mobileToolbarOpen = ref(false);
const pickerOpen = ref(false);
const menuOpen = ref(false);

function closeAllMenus() {
  mobileToolbarOpen.value = false;
  pickerOpen.value = false;
  menuOpen.value = false;
}

function react(emoji: string) {
  emit("react", props.message.id, emoji);
  closeAllMenus();
}

function startReplyFromMenu() {
  emit("reply", props.message);
  closeAllMenus();
}

function startEditFromMenu() {
  startEdit();
  closeAllMenus();
}

function retractFromMenu() {
  emit("retract", props.message.id);
  closeAllMenus();
}

function openPickerFromMenu() {
  menuOpen.value = false;
  pickerOpen.value = true;
}

const longPress = useLongPress({
  onLongPress: () => {
    mobileToolbarOpen.value = true;
  },
});

function onBubbleContextMenu(event: MouseEvent) {
  // Suppress iOS Safari / Android native long-press menu while the gesture is
  // being handled. Desktop right-click (pointerType 'mouse' never sets
  // isPressing) remains untouched.
  if (longPress.isPressing.value) event.preventDefault();
}

function onWindowPointerDown(event: PointerEvent) {
  if (!bubbleEl.value) return;
  const target = event.target as Node | null;
  if (target && bubbleEl.value.contains(target)) return;
  closeAllMenus();
}

onMounted(() => {
  window.addEventListener("pointerdown", onWindowPointerDown, true);
});

onUnmounted(() => {
  window.removeEventListener("pointerdown", onWindowPointerDown, true);
});

watch(
  styledHtml,
  () => {
    void nextTick().then(highlightMessageCodeBlocks);
  },
  { immediate: true },
);
</script>

<template>
  <!-- Retracted tombstone -->
  <div
    v-if="message.isRetracted"
    :data-message-id="message.id"
    class="flow-root px-3 py-2 opacity-30 animate-message-in"
  >
    <AppAvatar
      class="float-left mr-3 mt-0.5"
      :name="message.author"
      :src="avatarUrl"
      :presence="presence"
      :last-seen="lastSeen"
      size="md"
    />
    <div class="flex items-baseline gap-2 mb-0.5">
      <span class="font-medium text-[13px]">{{ message.author }}</span>
      <span class="text-[11px] font-mono text-muted-foreground tabular-nums">
        {{ formatStamp(message.createdAt) }}
      </span>
    </div>
    <p class="text-[13px] italic text-muted-foreground">This message was deleted.</p>
  </div>

  <!-- Normal message -->
  <div
    v-else
    ref="bubbleEl"
    :data-message-id="message.id"
    :data-mobile-open="mobileToolbarOpen || pickerOpen ? 'true' : 'false'"
    class="group relative flow-root px-3 py-1.5 rounded-xl transition-all duration-200 animate-message-in"
    :class="[
      isMentioned
        ? 'bg-warning/5 border-l-2 border-warning/30'
        : message.threadId
          ? 'border-l-2 border-primary/20 hover:bg-muted/40'
          : 'hover:bg-muted/40',
      message.deliveryStatus === 'sending' ? 'opacity-50' : '',
      longPress.isPressing.value ? 'no-callout' : '',
    ]"
    @pointerdown="longPress.handlers.onPointerdown"
    @pointermove="longPress.handlers.onPointermove"
    @pointerup="longPress.handlers.onPointerup"
    @pointercancel="longPress.handlers.onPointercancel"
    @pointerleave="longPress.handlers.onPointerleave"
    @contextmenu="onBubbleContextMenu"
  >
    <button
      class="float-left mr-3 mt-0.5 rounded-lg"
      type="button"
      @click.stop="emitAvatarClick"
    >
      <AppAvatar :name="message.author" :src="avatarUrl" :presence="presence" :last-seen="lastSeen" size="md" />
    </button>
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
        v-if="message.isSelf && message.readBy && message.readBy.length > 0"
        class="text-[11px] text-muted-foreground/50"
        :title="message.readBy.join(', ')"
      >
        Read by {{ message.readBy.length }}
      </span>
    </div>

    <!-- Reply preview chip -->
    <button
      v-if="message.replyTo"
      type="button"
      class="flex items-center gap-1.5 text-[11px] text-muted-foreground mb-0.5 hover:text-foreground transition-colors max-w-full"
      :title="`Jump to replied message`"
      @click="emit('scrollToMessage', message.replyTo.id)"
    >
      <CornerDownRight class="w-3 h-3 flex-shrink-0" />
      <span class="text-primary/80 font-medium">@{{ replyAuthorName }}</span>
      <span v-if="message.replyTo.preview" class="truncate opacity-70">{{ message.replyTo.preview }}</span>
      <span v-else class="opacity-60 font-mono">{{ message.replyTo.id.slice(0, 8) }}</span>
    </button>

    <!-- Edit mode -->
    <div v-if="isEditing" class="flex gap-2 mt-1 items-end">
      <ChatEditor
        :ref="setEditEditorRef"
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
    <div v-else-if="message.isSticker && imageAttachments.length > 0" class="mt-1">
      <img
        :src="imageAttachments[0].url"
        :alt="imageAttachments[0].desc ?? message.body ?? 'Sticker'"
        class="max-w-28 max-h-28 object-contain"
        loading="lazy"
      />
    </div>

    <template v-else>
      <!-- User text body (shown alongside attachments) -->
      <div
        v-if="displayBody"
        :ref="setStyledBodyRef"
        class="text-[13px] leading-relaxed break-words styled-body"
        v-html="styledHtml"
      />

      <!-- Inline GIF -->
      <div v-else-if="isGif" class="mt-2">
        <img
          :src="message.body.trim()"
          alt="GIF"
          class="max-w-xs max-h-56 rounded-xl border border-border object-contain"
          loading="lazy"
        />
      </div>

      <!-- Image attachments gallery -->
      <div v-if="imageAttachments.length > 0" class="mt-2 flex flex-wrap gap-2">
        <button
          v-for="(img, idx) in imageAttachments"
          :key="img.url"
          type="button"
          class="rounded-xl border border-border overflow-hidden hover:opacity-90 transition-opacity focus-visible:outline-2 focus-visible:outline-primary"
          :title="img.name ?? 'Image'"
          @click="openLightbox(idx)"
        >
          <img
            :src="img.url"
            :alt="img.name ?? 'Shared image'"
            class="max-w-xs max-h-56 object-cover"
            loading="lazy"
          />
        </button>
      </div>

      <!-- Non-image attachments -->
      <div v-if="nonImageAttachments.length > 0" class="mt-2 flex flex-col gap-1.5">
        <a
          v-for="file in nonImageAttachments"
          :key="file.url"
          :href="file.url"
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-3 bg-muted rounded-xl p-3 hover:bg-muted/80 transition-all duration-200 max-w-md"
        >
          <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
          <div class="flex-1 min-w-0">
            <div class="text-[13px] font-medium truncate">{{ file.name ?? "File" }}</div>
            <div class="text-[11px] text-muted-foreground">
              {{ file.mediaType ?? "file" }}
              <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
            </div>
          </div>
        </a>
      </div>
    </template>

    <ImageLightbox
      v-model:open="lightboxOpen"
      v-model:index="lightboxIndex"
      :images="lightboxImages"
    />

    <!-- Existing reactions (inline, always visible when present) -->
    <div v-if="message.reactions && Object.keys(message.reactions).length > 0" class="flex flex-wrap gap-1 mt-1.5">
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
    </div>

    <!-- Floating action toolbar — absolute so no layout space when hidden.
         Revealed by hover (mouse), focus-within (keyboard), or long-press (touch). -->
    <div
      v-if="!isEditing"
      class="absolute -top-3 right-3 z-10 opacity-0 group-hover:opacity-100 focus-within:opacity-100 group-data-[mobile-open=true]:opacity-100 pointer-events-none group-hover:pointer-events-auto focus-within:pointer-events-auto group-data-[mobile-open=true]:pointer-events-auto transition-opacity duration-150 flex items-center gap-0.5 bg-card/95 backdrop-blur border border-border rounded-lg shadow-lg p-1"
    >
      <button
        v-for="e in quickEmojis"
        :key="e"
        type="button"
        class="h-6 w-6 flex items-center justify-center text-[14px] leading-none rounded-md hover:bg-muted hover:scale-110 transition-all duration-150"
        :title="`React with ${e}`"
        :aria-label="`React to message with ${e}`"
        @click="react(e)"
      >{{ e }}</button>
      <div class="relative">
        <button
          type="button"
          class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
          :class="pickerOpen ? 'bg-muted text-foreground' : ''"
          title="Add reaction"
          aria-label="Add reaction"
          :aria-expanded="pickerOpen"
          aria-haspopup="dialog"
          @click="pickerOpen = !pickerOpen; mobileToolbarOpen = false"
        >
          <SmilePlus class="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <EmojiPicker
          :open="pickerOpen"
          @select="react"
          @close="pickerOpen = false"
        />
      </div>
      <button
        type="button"
        class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
        title="Reply"
        aria-label="Reply to message"
        @click="startReplyFromMenu"
      >
        <Reply class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
      <template v-if="message.isSelf">
        <div class="w-px h-4 bg-border mx-0.5" />
        <button
          type="button"
          class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
          title="Edit message"
          aria-label="Edit message"
          @click="startEditFromMenu"
        >
          <Pencil class="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-all duration-150"
          title="Delete message"
          aria-label="Delete message"
          @click="retractFromMenu"
        >
          <Trash2 class="w-3.5 h-3.5" aria-hidden="true" />
        </button>
      </template>
    </div>

    <!-- Accessible always-reachable affordance: keyboard / screen-reader users
         (and anyone who prefers a button to a long-press) get the same actions
         via a standard menu. Positioned at the top-right of the bubble, low
         contrast on desktop, prominent on touch where hover never fires. -->
    <div v-if="!isEditing" class="absolute top-1 right-1 z-10">
      <button
        type="button"
        class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted opacity-40 hover:opacity-100 focus-visible:opacity-100 group-data-[mobile-open=true]:opacity-100 transition-all duration-150"
        title="Message actions"
        aria-label="Message actions"
        :aria-expanded="menuOpen"
        aria-haspopup="menu"
        @click="menuOpen = !menuOpen; mobileToolbarOpen = false"
      >
        <MoreHorizontal class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
      <div
        v-if="menuOpen"
        role="menu"
        aria-label="Message actions"
        class="absolute top-full right-0 mt-1 min-w-40 glass-panel border border-border rounded-xl shadow-2xl p-1 animate-fade-in"
        @pointerdown.stop
      >
        <div class="flex items-center gap-0.5 px-1 py-1">
          <button
            v-for="e in quickEmojis"
            :key="`menu-${e}`"
            type="button"
            role="menuitem"
            class="h-8 w-8 flex items-center justify-center text-[16px] leading-none rounded-md hover:bg-muted transition-colors"
            :aria-label="`React with ${e}`"
            @click="react(e)"
          >{{ e }}</button>
        </div>
        <button
          type="button"
          role="menuitem"
          class="w-full flex items-center gap-2 px-2 py-1.5 text-[13px] rounded-md hover:bg-muted transition-colors text-left"
          @click="openPickerFromMenu"
        >
          <SmilePlus class="w-3.5 h-3.5" aria-hidden="true" />
          <span>More reactions…</span>
        </button>
        <button
          type="button"
          role="menuitem"
          class="w-full flex items-center gap-2 px-2 py-1.5 text-[13px] rounded-md hover:bg-muted transition-colors text-left"
          @click="startReplyFromMenu"
        >
          <Reply class="w-3.5 h-3.5" aria-hidden="true" />
          <span>Reply</span>
        </button>
        <template v-if="message.isSelf">
          <div class="my-1 h-px bg-border" />
          <button
            type="button"
            role="menuitem"
            class="w-full flex items-center gap-2 px-2 py-1.5 text-[13px] rounded-md hover:bg-muted transition-colors text-left"
            @click="startEditFromMenu"
          >
            <Pencil class="w-3.5 h-3.5" aria-hidden="true" />
            <span>Edit</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="w-full flex items-center gap-2 px-2 py-1.5 text-[13px] rounded-md hover:bg-destructive/10 text-destructive transition-colors text-left"
            @click="retractFromMenu"
          >
            <Trash2 class="w-3.5 h-3.5" aria-hidden="true" />
            <span>Delete</span>
          </button>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.styled-body :deep(pre.message-code-block) {
  margin: 0.5rem 0;
  padding: 0.75rem 0.9rem;
  border-radius: 0.75rem;
  border: 1px solid var(--border);
  overflow-x: auto;
}

</style>
