<script setup lang="ts">
import Skeleton from "@/components/ui/Skeleton.vue";

type MessageSkeletonRow = {
  id: number;
  grouped: boolean;
  authorWidth: string;
  lines: string[];
};

const skeletonMessageRows: ReadonlyArray<MessageSkeletonRow> = Object.freeze([
  { id: 1, grouped: false, authorWidth: "5rem",   lines: ["72%"] },
  { id: 2, grouped: true,  authorWidth: "",       lines: ["55%", "38%"] },
  { id: 3, grouped: false, authorWidth: "6.5rem", lines: ["88%", "63%", "41%"] },
  { id: 4, grouped: false, authorWidth: "4.5rem", lines: ["49%"] },
  { id: 5, grouped: true,  authorWidth: "",       lines: ["66%"] },
  { id: 6, grouped: false, authorWidth: "5.75rem", lines: ["78%", "52%"] },
]);
</script>

<template>
  <div class="chat-message-lane flex flex-col gap-1 py-6" aria-busy="true" aria-label="Loading messages">
    <div
      v-for="row in skeletonMessageRows"
      :key="`msg-skel-${row.id}`"
      class="chat-message-grid"
      :class="row.grouped ? 'chat-message-grouped' : ''"
    >
      <div class="chat-message-avatar-cell">
        <Skeleton
          v-if="!row.grouped"
          width="var(--chat-message-avatar-size)"
          height="var(--chat-message-avatar-size)"
          radius="var(--radius-md)"
        />
      </div>
      <div class="chat-message-body-stack flex flex-col gap-1.5">
        <div v-if="!row.grouped" class="flex items-center gap-2">
          <Skeleton :width="row.authorWidth" height="0.65rem" />
          <Skeleton width="2.25rem" height="0.55rem" />
        </div>
        <Skeleton
          v-for="(width, i) in row.lines"
          :key="`msg-skel-line-${row.id}-${i}`"
          :width="width"
          height="0.7rem"
        />
      </div>
    </div>
  </div>
</template>
