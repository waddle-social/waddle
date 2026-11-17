<script setup lang="ts">
import { css } from "styled-system/css";
import { btn, waddle } from "styled-system/recipes";
import type { ConnectionNoticeCopy } from "@/lib/connection-notice";
import { connectionNoticeClasses as cls, connectionToneClasses } from "./composables/use-connection-notice";

/**
 * The connection notice card (Connection-Notices board): the state's
 * accent border, the waddle spinner while reconnecting, and an optional
 * primary action. It floats over the timeline below the header instead
 * of pushing it down.
 */
defineProps<{ notice: ConnectionNoticeCopy }>();
defineEmits<{ action: [] }>();

const anchor = css({ position: "relative", height: 0, zIndex: "sticky" });
const float = css({ position: "absolute", top: 0, left: 0, right: 0 });
</script>

<template>
  <div :class="anchor">
    <div :class="float">
      <div role="status" aria-live="polite" aria-atomic="true" :class="[cls.card, connectionToneClasses[notice.actionKind === 'recover-superseded' ? 'reconnecting' : notice.tone].banner]">
        <span v-if="notice.tone === 'reconnecting'" :class="waddle()" aria-hidden="true"><i /><i /><i /></span>
        <div :class="cls.text">
          <p :class="cls.title">{{ notice.title }}</p>
          <p :class="cls.body">{{ notice.body }}</p>
        </div>
        <button v-if="notice.actionLabel" type="button" :class="btn({ variant: 'primary', size: 'sm' })" @click="$emit('action')">
          <span>{{ notice.actionLabel }}</span>
        </button>
      </div>
    </div>
  </div>
</template>
