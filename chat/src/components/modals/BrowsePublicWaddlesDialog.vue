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
    <div class="flex h-14 items-center justify-between border-b border-border px-4 sm:px-5">
      <h2 class="text-[16px] font-display font-bold">Browse Public Spaces</h2>
      <button class="flex h-8 w-8 items-center justify-center rounded-lg hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="space-y-2 border-b border-border px-4 py-3 sm:px-5">
      <div class="relative">
        <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
        <input
          :value="query"
          class="h-9 w-full rounded-lg bg-muted pl-9 pr-3 text-[13px] transition-all duration-200 focus:outline-none focus:ring-2 focus:ring-primary/20"
          placeholder="Search by name or id..."
          @input="emit('update:query', ($event.target as HTMLInputElement).value)"
          @keydown.enter.prevent="emit('refresh')"
        />
      </div>

      <button
        class="h-9 rounded-lg border border-border px-3 text-[13px] font-medium hover:bg-muted transition-all duration-200"
        :disabled="isLoading"
        @click="emit('refresh')"
      >
        {{ isLoading ? "Loading..." : "Search" }}
      </button>
    </div>

    <div class="max-h-80 space-y-2 overflow-auto px-4 py-3 sm:px-5">
      <div
        v-for="space in spaces"
        :key="space.id"
        class="flex items-start gap-3 rounded-lg border border-border bg-surface p-3"
      >
        <div class="flex-1 min-w-0">
          <div class="font-medium text-[13px] truncate">
            {{ space.name }}
          </div>
          <div class="text-[11px] text-muted-foreground font-mono truncate">
            {{ space.id }}
          </div>
          <p v-if="space.description" class="text-[13px] mt-1 text-muted-foreground">
            {{ space.description }}
          </p>
        </div>

        <button
          class="h-8 rounded-lg px-2.5 text-[12px] font-medium transition-all duration-200 disabled:opacity-40"
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
                ? "Joining..."
                : "Join"
          }}
        </button>
      </div>

      <div v-if="!isLoading && spaces.length === 0" class="text-center py-6 text-[13px] text-muted-foreground">
        No public spaces found
      </div>
    </div>
  </AppDialog>
</template>
