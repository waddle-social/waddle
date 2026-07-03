<script setup lang="ts">
import { computed } from "vue";
import { ExternalLink, Github, LayoutDashboard, MessageSquare } from "lucide-vue-next";
import type { ExtensionAnnotationAction } from "@/lib/chat-ui";
import { useExtensionAnnotationActions } from "@/channels/extension-annotation-actions";
import { formatTimelineTimeOfDay } from "@/channels/timeline";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import {
  systemBandKindClass,
  systemBandMetaValueClass,
  systemBandToneClass,
  type SystemBandCard,
} from "./message-system-band";

const props = defineProps<{
  card: SystemBandCard;
  messageId: string;
  messageCreatedAt: string;
  messageAuthor: string;
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>;
}>();

const bandIcon = computed(() => {
  if (props.card.presentation.kind === "github-event") return Github;
  if (props.card.annotation.surfaceKind === "chat-bot") return MessageSquare;
  return LayoutDashboard;
});

// Reuse the same action-state machine MessageBody uses so loading /
// success / error feedback is consistent across all extension surfaces.
const cardAnnotations = computed(() => [props.card.annotation]);
const invokeExtensionActionRef = computed(() => props.invokeExtensionAction);
const {
  actionState: bandActionState,
  invokeExtension: invokeBandAction,
} = useExtensionAnnotationActions({
  annotations: cardAnnotations,
  invokeExtensionAction: invokeExtensionActionRef,
});
</script>

<template>
  <section
    :data-message-id="messageId"
    :data-message-created-at="messageCreatedAt"
    class="chat-system-band animate-message-in"
    :class="[
      systemBandToneClass(card.presentation.tone),
      systemBandKindClass(card.presentation.kind),
    ]"
  >
    <div class="chat-system-band__header">
      <span class="chat-system-band__source">
        <component :is="bandIcon" aria-hidden="true" />
        {{ card.presentation.label || messageAuthor }}
      </span>
      <span class="chat-system-band__stamp">{{ formatTimelineTimeOfDay(messageCreatedAt) }}</span>
      <span v-if="card.presentation.primaryValue" class="chat-system-band__tone-pill">
        {{ card.presentation.primaryValue }}
      </span>
    </div>
    <div class="chat-system-band__title">
      <a
        v-if="card.presentation.primaryUrl"
        :href="card.presentation.primaryUrl"
        target="_blank"
        rel="noopener noreferrer"
        class="chat-system-band__title-link"
        @click.stop
      >
        <span>{{ card.presentation.title }}</span>
        <ExternalLink aria-hidden="true" />
      </a>
      <template v-else>{{ card.presentation.title }}</template>
    </div>
    <div
      v-if="card.presentation.details.length > 0 || card.presentation.secondaryValue"
      class="chat-system-band__meta"
    >
      <span v-if="card.presentation.secondaryValue" class="chat-system-band__meta-item">
        <span class="chat-system-band__meta-value">{{ card.presentation.secondaryValue }}</span>
      </span>
      <span
        v-for="detail in card.presentation.details"
        :key="`${card.annotation.annotationId}:${detail.label}`"
        class="chat-system-band__meta-item"
      >
        <span class="chat-system-band__meta-label">{{ detail.label }}</span>
        <span
          class="chat-system-band__meta-value"
          :class="systemBandMetaValueClass(detail.label)"
          :title="detail.value"
        >{{ detail.value }}</span>
      </span>
    </div>
    <div v-if="card.annotation.actions.length > 0" class="chat-system-band__actions">
      <button
        v-for="action in card.annotation.actions"
        :key="`${card.annotation.annotationId}:${action.route}`"
        type="button"
        class="chat-extension-action-chip"
        :class="bandActionState(card.annotation.annotationId, action)?.state
          ? `chat-extension-action-chip--state-${bandActionState(card.annotation.annotationId, action)?.state}`
          : ''"
        :disabled="bandActionState(card.annotation.annotationId, action)?.state === 'loading' || !action.launch"
        :title="bandActionState(card.annotation.annotationId, action)?.detail ?? action.launch?.commandNode ?? action.label"
        @click.stop="invokeBandAction(card.annotation.annotationId, action)"
      >
        {{ action.label }}
      </button>
    </div>
  </section>
</template>
