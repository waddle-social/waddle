<script setup lang="ts">
import { ref } from "vue";
import { Check, CheckCheck, Pencil, Trash2 } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import { formatStamp } from "@/composables/useMessaging";

const props = defineProps<{
  message: TimelineMessage;
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string];
  retract: [messageId: string];
}>();

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
    class="group flex gap-4 border border-foreground p-4 transition-colors"
    :class="message.isSelf ? 'bg-muted/30' : ''"
  >
    <AppAvatar :name="message.author" size="md" />
    <div class="flex-1 min-w-0">
      <div class="flex items-baseline gap-3 mb-1">
        <span class="font-mono font-bold text-sm">{{ message.author }}</span>
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

      <!-- Normal display -->
      <p v-else class="text-sm leading-relaxed whitespace-pre-wrap break-words">{{ message.body }}</p>
    </div>
  </div>
</template>
