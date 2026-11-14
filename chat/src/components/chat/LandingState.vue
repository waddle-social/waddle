<script setup lang="ts">
import { css, cx } from "styled-system/css";
import { btn, waddle } from "styled-system/recipes";
import spot from "@waddle/design/assets/spot-conversation.svg?url";

/**
 * Full-page landing (App-SignIn board, landing states): "Checking
 * session." with the waddle spinner, or "Server unavailable." with the
 * error copy and a retry action.
 */
defineProps<{
  title: string;
  copy?: string;
  actionLabel?: string;
}>();

defineEmits<{
  action: [];
}>();

const page = css({ minHeight: "100dvh", display: "flex", alignItems: "center", justifyContent: "center", px: "4", py: "12", boxSizing: "border-box", bg: "bg", color: "fg" });
const column = css({ width: "100%", maxWidth: "384px", display: "flex", flexDirection: "column", alignItems: "center", gap: "3", textAlign: "center", animation: "wd-rise {durations.slow} {easings.out} backwards" });
const spotClass = css({ width: "120px", height: "120px", mb: "2" });
const titleClass = css({ margin: 0, font: "700 32px/1.15 {fonts.display}", letterSpacing: "-0.02em" });
const copyClass = css({ margin: 0, fontSize: "md", color: "fg.muted" });
const actionClass = cx(btn({ variant: "primary" }), css({ height: "44px", minWidth: "160px", justifyContent: "center" }));
const spinner = waddle();
</script>

<template>
  <div :class="page">
    <section :class="column">
      <img :src="spot" alt="Waddle" :class="spotClass" width="120" height="120" />
      <h1 :class="titleClass">{{ title }}</h1>
      <p v-if="copy" :class="copyClass">{{ copy }}</p>
      <button v-if="actionLabel" :class="actionClass" type="button" @click="$emit('action')">
        {{ actionLabel }}
      </button>
      <span v-else :class="spinner" aria-hidden="true"><i /><i /><i /></span>
    </section>
  </div>
</template>
