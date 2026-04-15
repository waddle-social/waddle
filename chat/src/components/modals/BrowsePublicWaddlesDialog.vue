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
    <div class="border-b border-border px-5 py-4 flex items-center justify-between">
      <h2 class="text-[15px] font-semibold">Browse Public Spaces</h2>
      <button class="p-1 rounded-md hover:bg-muted transition-colors" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="px-5 py-3 border-b border-border space-y-2">
      <div class="relative">
        <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
        <input
          :value="query"
          class="w-full border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring pl-9 pr-3 py-2 bg-surface text-[13px] transition-shadow"
          placeholder="Search by name or id..."
          @input="emit('update:query', ($event.target as HTMLInputElement).value)"
          @keydown.enter.prevent="emit('refresh')"
        />
      </div>

      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-md border border-border hover:bg-muted transition-colors"
        :disabled="isLoading"
        @click="emit('refresh')"
      >
        {{ isLoading ? "Loading..." : "Search" }}
      </button>
    </div>

    <div class="px-5 py-3 max-h-80 overflow-auto space-y-1">
      <div
        v-for="space in spaces"
        :key="space.id"
        class="rounded-md bg-surface border border-border p-3 flex items-start gap-3"
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
          class="text-[12px] font-medium py-1.5 px-2.5 rounded-md transition-all disabled:opacity-40"
          :class="joinedSet.has(space.id)
            ? 'bg-muted text-muted-foreground'
            : 'bg-primary text-primary-foreground hover:opacity-90'"
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
