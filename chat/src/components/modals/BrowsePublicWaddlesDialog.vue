<script setup lang="ts">
import { computed } from "vue";
import { Search, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { WaddleSummary } from "@/lib/waddle-api";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  spaces: WaddleSummary[];
  joinedWaddleIds: string[];
  isLoading: boolean;
  joiningWaddleId: string | null;
  query: string;
}>();

const emit = defineEmits<{
  "update:query": [value: string];
  refresh: [];
  join: [waddleId: string];
}>();

const joinedSet = computed(() => new Set(props.joinedWaddleIds));
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">Browse public spaces</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close browse public spaces dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-panel-stack border-b border-border px-4 py-3 sm:px-5">
      <div class="relative">
        <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
        <input
          :value="query"
          class="chat-field-control chat-field-control--search type-field"
          placeholder="Search by name or id…"
          aria-label="Search public spaces"
          @input="emit('update:query', ($event.target as HTMLInputElement).value)"
          @keydown.enter.prevent="emit('refresh')"
        />
      </div>

      <button
        class="chat-action-button chat-action-button--secondary type-control"
        type="button"
        :disabled="isLoading"
        @click="emit('refresh')"
      >
        {{ isLoading ? "Loading…" : "Search" }}
      </button>
    </div>

    <div class="chat-panel-stack max-h-80 overflow-auto px-4 py-3 sm:px-5">
      <div
        v-for="space in spaces"
        :key="space.id"
        class="flex items-start gap-3 rounded-lg border border-border bg-surface p-3"
      >
        <div class="flex-1 min-w-0">
          <div class="type-control truncate">
            {{ space.name }}
          </div>
          <div class="type-caption type-mono truncate text-muted-foreground">
            {{ space.id }}
          </div>
          <p v-if="space.description" class="type-caption pt-1 text-muted-foreground">
            {{ space.description }}
          </p>
        </div>

        <button
          class="chat-action-button type-control min-h-8 px-2.5 disabled:opacity-40"
          type="button"
          :aria-label="joinedSet.has(space.id) ? `${space.name} already joined` : `Join ${space.name}`"
          :class="joinedSet.has(space.id)
            ? 'bg-muted text-muted-foreground'
            : 'bg-primary text-primary-foreground hover:shadow-[0_0_12px_var(--glow)]'"
          :disabled="joinedSet.has(space.id) || joiningWaddleId === space.id"
          @click="emit('join', space.id)"
        >
          {{
            joinedSet.has(space.id)
              ? "Joined"
              : joiningWaddleId === space.id
                ? "Joining…"
                : "Join"
          }}
        </button>
      </div>

      <div v-if="!isLoading && spaces.length === 0" class="type-caption text-center py-6 text-muted-foreground">
        No public spaces found
      </div>
    </div>
  </AppDialog>
</template>
