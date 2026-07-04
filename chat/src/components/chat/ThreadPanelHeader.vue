<script setup lang="ts">
import { computed } from "vue";
import { X, ChevronRight, ChevronLeft } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import CallAnchorCard from "@/components/calls/CallAnchorCard.vue";
import type { CallAnchorCardState } from "@/lib/call-thread-anchor";
import {
  overflowThreadParticipantCount,
  visibleThreadParticipants,
  type ThreadParticipant,
} from "./thread-lobby-meta";

// Thread lobby header — substantial metadata that helps you orient inside a
// thread. Top row: preview of the root message (or breadcrumb when nested).
// Bottom row: who started it · N replies · when it last moved · who's been
// participating · close. When the stack is deeper than one thread the top
// row becomes a breadcrumb trail so the user can pop back through nested
// sub-threads. One breadcrumb label per stack entry, so the labels double
// as the stack depth.
const props = defineProps<{
  breadcrumbLabels: string[];
  threadPreview: string;
  callAnchorState: CallAnchorCardState | null;
  rootAuthor: string | null;
  replyCount: number;
  lastActivityLabel: string;
  participants: ThreadParticipant[];
}>();

const emit = defineEmits<{
  close: [];
  popTo: [index: number];
  joinCall: [];
  openThread: [threadId: string];
}>();

const visibleParticipants = computed(() => visibleThreadParticipants(props.participants));
const overflowParticipants = computed(() => overflowThreadParticipantCount(props.participants));
</script>

<template>
  <div class="chat-thread-header flex-shrink-0 glass-panel">
    <div class="chat-thread-header__row chat-thread-header__row--top">
      <template v-if="breadcrumbLabels.length > 1">
        <div class="chat-thread-header__breadcrumb type-caption">
          <span class="chat-thread-header__breadcrumb-label">Threads</span>
          <template v-for="(label, i) in breadcrumbLabels" :key="i">
            <ChevronRight class="chat-thread-header__breadcrumb-sep" aria-hidden="true" />
            <button
              type="button"
              class="chat-thread-header__breadcrumb-crumb"
              :class="i === breadcrumbLabels.length - 1 ? 'chat-thread-header__breadcrumb-crumb--active' : ''"
              :title="label"
              @click="emit('popTo', i)"
            >{{ label }}</button>
          </template>
        </div>
      </template>
      <template v-else>
        <p
          v-if="threadPreview"
          class="chat-thread-header__preview"
          :title="threadPreview"
        >{{ threadPreview }}</p>
        <p v-else class="chat-thread-header__preview chat-thread-header__preview--empty">Thread</p>
      </template>
      <div class="chat-thread-header__actions">
        <button
          v-if="breadcrumbLabels.length > 1"
          type="button"
          class="chat-icon-button hover:bg-muted lg:hidden"
          title="Go back"
          aria-label="Go back"
          @click="emit('popTo', breadcrumbLabels.length - 2)"
        >
          <ChevronLeft class="w-4 h-4" />
        </button>
        <button
          type="button"
          class="chat-icon-button hover:bg-muted"
          title="Close thread"
          aria-label="Close thread"
          @click="emit('close')"
        >
          <X class="w-4 h-4" />
        </button>
      </div>
    </div>
    <div v-if="callAnchorState" class="chat-thread-header__row">
      <CallAnchorCard
        :state="callAnchorState"
        class="w-full"
        @join="emit('joinCall')"
        @open-thread="(threadId) => emit('openThread', threadId)"
      />
    </div>
    <div class="chat-thread-header__row chat-thread-header__row--meta">
      <div class="chat-thread-header__meta type-caption">
        <span v-if="rootAuthor" class="chat-thread-header__meta-item">
          <span class="chat-thread-header__meta-label">Started by</span>
          <span class="chat-thread-header__meta-value">{{ rootAuthor }}</span>
        </span>
        <span class="chat-thread-header__meta-item">
          <span class="chat-thread-header__meta-value">
            <strong>{{ replyCount }}</strong>
            {{ replyCount === 1 ? "reply" : "replies" }}
          </span>
        </span>
        <span v-if="lastActivityLabel" class="chat-thread-header__meta-item">
          <span class="chat-thread-header__pulse" aria-hidden="true" />
          <span class="chat-thread-header__meta-value">active {{ lastActivityLabel }}</span>
        </span>
      </div>
      <div v-if="visibleParticipants.length > 0" class="chat-thread-header__participants">
        <span class="chat-thread-header__avatars">
          <span
            v-for="participant in visibleParticipants"
            :key="`thread-participant:${participant.nick}`"
            class="chat-thread-header__avatar-wrap"
            :title="`${participant.nick}`"
          >
            <AppAvatar
              :name="participant.nick"
              :src="participant.avatarUrl ?? null"
              :presence="participant.presence"
              size="xs"
            />
          </span>
        </span>
        <span v-if="overflowParticipants > 0" class="chat-thread-header__overflow">+{{ overflowParticipants }}</span>
      </div>
    </div>
  </div>
</template>
