<script setup lang="ts">
import Skeleton from "@/components/ui/Skeleton.vue";
import { css } from "styled-system/css";
import { msg } from "styled-system/recipes";

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
const listClass = css({ display: "flex", flexDirection: "column", gap: "0.5", py: "4" });
const linesClass = css({ gridColumn: 2, display: "flex", flexDirection: "column", gap: "1.5", pt: "1" });
const metaClass = css({ display: "flex", alignItems: "center", gap: "2" });
</script>

<template>
  <div :class="listClass" aria-busy="true" aria-label="Loading messages">
    <div
      v-for="row in skeletonMessageRows"
      :key="`msg-skel-${row.id}`"
      :class="msg({ cont: row.grouped })"
    >
      <Skeleton
        v-if="!row.grouped"
        width="var(--sizes-msg-avatar)"
        height="var(--sizes-msg-avatar)"
        radius="var(--radii-pill)"
      />
      <span v-else />
      <div :class="linesClass">
        <div v-if="!row.grouped" :class="metaClass">
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
