<script setup lang="ts">
import { computed } from "vue";
import { X, ChevronRight, ChevronLeft } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
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

// ThreadPanel board: one header bar (back when nested, title, close), then a
// caption line with who started it, the reply count, recency and faces.
// The `chat-thread-header__*` names are unstyled hooks the tests read.
const rootClass = css({ display: "flex", flexDirection: "column", gap: "2", flex: "none", padding: "10px 12px 12px 16px", borderBottom: "1px solid {colors.border.soft}" });
const topClass = css({ display: "flex", alignItems: "center", gap: "2", minWidth: 0, minHeight: "40px" });
const titleClass = css({ flex: 1, minWidth: 0, margin: 0, font: "700 16px/1.3 {fonts.display}", color: "fg", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const crumbsClass = css({ flex: 1, minWidth: 0, display: "flex", alignItems: "center", gap: "1", overflow: "hidden", fontSize: "13px", color: "fg.muted", "& > svg": { width: "14px", height: "14px", flex: "none", color: "fg.soft" } });
const crumbLabelClass = css({ flex: "none", fontWeight: 600 });
const crumbClass = css({
  minWidth: 0,
  maxWidth: "12rem",
  overflow: "hidden",
  textOverflow: "ellipsis",
  whiteSpace: "nowrap",
  padding: "2px 6px",
  border: 0,
  borderRadius: "tag",
  bg: "transparent",
  color: "fg.muted",
  font: "inherit",
  cursor: "pointer",
  _hover: { bg: "state.hover", color: "fg" },
  _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
});
const crumbActiveClass = cx(crumbClass, css({ color: "fg", fontFamily: "display", fontWeight: 700, fontSize: "15px" }));
const actionsClass = css({ display: "flex", alignItems: "center", gap: "0.5", flex: "none", ml: "auto" });
const iconClass = iconbtn();
const metaRowClass = css({ display: "flex", alignItems: "center", gap: "3", minWidth: 0 });
const metaClass = css({ flex: 1, minWidth: 0, display: "flex", flexWrap: "wrap", alignItems: "center", gap: "1 0", fontSize: "13px", color: "fg.muted" });
const metaItemClass = css({ display: "inline-flex", alignItems: "center", gap: "1.5", "& + &::before": { content: '"·"', mx: "1.5", color: "fg.soft" }, "& strong": { color: "fg", fontWeight: 600, fontVariantNumeric: "tabular-nums" } });
const metaValueClass = css({ color: "fg" });
const pulseClass = css({ width: "8px", height: "8px", borderRadius: "pill", bg: "live", flex: "none" });
const facesClass = css({ display: "flex", alignItems: "center", gap: "1.5", flex: "none" });
const faceStackClass = css({ display: "flex", "& > span": { ml: "-6px", borderRadius: "pill", boxShadow: "0 0 0 2px {colors.bg.rail}" }, "& > span:first-child": { ml: 0 } });
const overflowClass = css({ display: "inline-flex", alignItems: "center", justifyContent: "center", minWidth: "24px", height: "24px", px: "1.5", borderRadius: "pill", border: "1px solid {colors.border}", bg: "surface.2", fontSize: "11px", fontWeight: 600, color: "fg.muted", fontVariantNumeric: "tabular-nums" });

const visibleParticipants = computed(() => visibleThreadParticipants(props.participants));
const overflowParticipants = computed(() => overflowThreadParticipantCount(props.participants));
</script>

<template>
  <header class="chat-thread-header" :class="rootClass">
    <div class="chat-thread-header__row chat-thread-header__row--top" :class="topClass">
      <AppTooltip v-if="breadcrumbLabels.length > 1" label="Go back" placement="bottom">
        <button
          type="button"
          :class="iconClass"
          aria-label="Go back"
          @click="emit('popTo', breadcrumbLabels.length - 2)"
        >
          <ChevronLeft aria-hidden="true" />
        </button>
      </AppTooltip>
      <template v-if="breadcrumbLabels.length > 1">
        <div class="chat-thread-header__breadcrumb" :class="crumbsClass">
          <span :class="crumbLabelClass">Threads</span>
          <template v-for="(label, i) in breadcrumbLabels" :key="i">
            <ChevronRight aria-hidden="true" />
            <AppTooltip :label="label" placement="bottom">
              <button
                type="button"
                :class="i === breadcrumbLabels.length - 1 ? ['chat-thread-header__breadcrumb-crumb--active', crumbActiveClass] : crumbClass"
                :aria-current="i === breadcrumbLabels.length - 1 ? 'location' : undefined"
                @click="emit('popTo', i)"
              >{{ label }}</button>
            </AppTooltip>
          </template>
        </div>
      </template>
      <template v-else>
        <p
          v-if="threadPreview"
          class="chat-thread-header__preview"
          :class="titleClass"
          :title="threadPreview"
        >{{ threadPreview }}</p>
        <p v-else class="chat-thread-header__preview chat-thread-header__preview--empty" :class="titleClass">Thread</p>
      </template>
      <div :class="actionsClass">
        <AppTooltip label="Close thread" placement="bottom">
          <button
            type="button"
            :class="iconClass"
            aria-label="Close thread"
            @click="emit('close')"
          >
            <X aria-hidden="true" />
          </button>
        </AppTooltip>
      </div>
    </div>
    <div v-if="callAnchorState">
      <CallAnchorCard
        :state="callAnchorState"
        @join="emit('joinCall')"
        @open-thread="(threadId) => emit('openThread', threadId)"
      />
    </div>
    <div :class="metaRowClass">
      <div :class="metaClass">
        <span v-if="rootAuthor" :class="metaItemClass">
          <span>Started by</span>
          <span :class="metaValueClass">{{ rootAuthor }}</span>
        </span>
        <span :class="metaItemClass">
          <span>
            <strong>{{ replyCount }}</strong>
            {{ replyCount === 1 ? "reply" : "replies" }}
          </span>
        </span>
        <span v-if="lastActivityLabel" :class="metaItemClass">
          <span class="chat-thread-header__pulse" :class="pulseClass" aria-hidden="true" />
          <span>active {{ lastActivityLabel }}</span>
        </span>
      </div>
      <div v-if="visibleParticipants.length > 0" class="chat-thread-header__participants" :class="facesClass">
        <span :class="faceStackClass">
          <span
            v-for="participant in visibleParticipants"
            :key="`thread-participant:${participant.key}`"
            class="chat-thread-header__avatar-wrap"
            :title="`${participant.nick}`"
          >
            <UserAvatar
              :name="participant.nick"
              :jid="participant.jid"
              :presence="participant.presence"
              size="xs"
            />
          </span>
        </span>
        <span v-if="overflowParticipants > 0" class="chat-thread-header__overflow" :class="overflowClass">+{{ overflowParticipants }}</span>
      </div>
    </div>
  </header>
</template>
