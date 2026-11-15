<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { ChevronLeft, ChevronRight, X } from "lucide-vue-next";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";
import type { Story, StoryReactionSummary } from "@/lib/xmpp-client";
import { jidLocalpart } from "@/lib/xmpp/jid";
import { css } from "styled-system/css";
import { btn, chip, iconbtn, scrim } from "styled-system/recipes";

const ui = {
  positioner: css({
    position: "fixed",
    inset: 0,
    zIndex: "dialog",
    display: "flex",
    alignItems: { base: "flex-end", md: "center" },
    justifyContent: "center",
    padding: { base: 0, md: "6" },
  }),
  scrim: scrim(),
  dialog: css({
    position: "relative",
    zIndex: "dialog",
    boxSizing: "border-box",
    display: "flex",
    flexDirection: "column",
    width: { base: "100%", md: "672px" },
    maxWidth: "100%",
    maxHeight: { base: "92dvh", md: "calc(100dvh - 3rem)" },
    overflow: "hidden",
    borderRadius: { base: "24px 24px 0 0", md: "card" },
    bg: "surface",
    border: "1px solid {colors.border}",
    borderBottomWidth: { base: 0, md: "1px" },
    boxShadow: "overlay",
    color: "fg",
    outline: "none",
  }),
  grabber: css({ display: { base: "block", md: "none" }, alignSelf: "center", mt: "2", width: "40px", height: "4px", borderRadius: "pill", bg: "border" }),
  head: css({ display: "flex", alignItems: "center", gap: "2.5", padding: { base: "8px 12px 10px 16px", md: "14px 16px" } }),
  who: css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", lineHeight: 1.3, "& b": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" } }),
  caption: css({ textStyle: "caption", color: "fg.muted" }),
  close: iconbtn(),
  scroll: css({ minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column" }),
  media: css({
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    margin: { base: "0 12px", md: "0 16px" },
    height: { base: "380px", md: "420px" },
    maxHeight: "56dvh",
    borderRadius: "14px",
    overflow: "hidden",
    bg: "sea.shallow",
    "& img": { height: "92%", maxWidth: "92%", objectFit: "contain" },
    "& video": { width: "100%", height: "100%", bg: "sea.deep" },
  }),
  body: css({ margin: { base: "12px 16px 0", md: "14px 18px 0" }, fontSize: "15px", lineHeight: 1.5, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }),
  reactions: css({ display: "flex", flexDirection: "column", gap: "2", padding: { base: "12px 16px 4px", md: "14px 16px" } }),
  quick: css({ display: "flex", flexWrap: "wrap", gap: "2", justifyContent: { base: "space-between", md: "flex-start" } }),
  quickBtn: css({
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    width: "44px",
    height: "44px",
    border: "1px solid {colors.border}",
    borderRadius: "pill",
    bg: "surface",
    fontSize: "18px",
    cursor: "pointer",
    _hover: { borderColor: "border.strong", bgImage: "linear-gradient({colors.state.hover}, {colors.state.hover})" },
    _pressed: { bg: "action.subtle", borderColor: "action" },
  }),
  summary: css({ display: "flex", flexWrap: "wrap", gap: "1" }),
  chip: chip(),
  foot: css({ display: "flex", alignItems: "center", gap: "2", padding: { base: "10px 16px 28px", md: "12px 16px" }, borderTop: "1px solid {colors.border.soft}", "& .wd-btn": { height: { base: "44px", md: "40px" } } }),
  counter: css({ flex: 1, textAlign: "center", font: "600 13px/1 {fonts.mono}", color: "fg.muted" }),
};
const navButton = btn();

interface StoryReactionEntry {
  emoji: string;
  count: number;
  reactors: readonly string[];
}

const props = defineProps<{
  story: Story | null;
  storyIndex: number | null;
  storyCount: number;
  reactionSummary: StoryReactionSummary | null;
  reactionEntries: readonly StoryReactionEntry[];
}>();

const emit = defineEmits<{
  close: [];
  previous: [];
  next: [];
  react: [emoji: string];
}>();

const dialogEl = ref<HTMLElement | null>(null);
const previouslyFocused = ref<HTMLElement | null>(null);

const hasPrevious = computed(() => props.storyIndex !== null && props.storyIndex > 0);
const hasNext = computed(() => props.storyIndex !== null && props.storyIndex < props.storyCount - 1);

watch(
  () => props.story,
  async (story, previous) => {
    if (story && !previous) {
      previouslyFocused.value = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      await nextTick();
      dialogEl.value?.focus();
    } else if (!story && previous) {
      previouslyFocused.value?.focus();
      previouslyFocused.value = null;
    }
  },
);

function listedAgo(story: Story): string {
  if (typeof story.postedMs !== "number") return "";
  const delta = Math.max(0, Date.now() - story.postedMs);
  if (delta < 60_000) return "just now";
  if (delta < 3_600_000) return `${plural(Math.floor(delta / 60_000), "minute")} ago`;
  if (delta < 86_400_000) return `${plural(Math.floor(delta / 3_600_000), "hour")} ago`;
  if (delta < 604_800_000) return `${plural(Math.floor(delta / 86_400_000), "day")} ago`;
  return new Date(story.postedMs).toLocaleDateString();
}

function plural(value: number, unit: string): string {
  return `${value} ${unit}${value === 1 ? "" : "s"}`;
}

function authorLabel(author: string | undefined): string {
  if (!author) return "Anonymous";
  return jidLocalpart(author);
}

function isVideoStory(story: Story): boolean {
  return Boolean(story.mediaUrl && /\.(mp4|webm|mov)(\?|$)/i.test(story.mediaUrl));
}

function eventTargetUsesArrows(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && Boolean(target.closest("video,audio,input,textarea,select,[role='combobox'],[role='listbox'],[contenteditable='true']"));
}

function focusableElements(): HTMLElement[] {
  const root = dialogEl.value;
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(
    "button:not([disabled]), a[href], input:not([disabled]), textarea:not([disabled]), select:not([disabled]), video[controls], [tabindex]:not([tabindex='-1'])",
  )).filter((el) => !el.hasAttribute("disabled") && el.tabIndex !== -1);
}

function trapTab(event: KeyboardEvent) {
  const focusables = focusableElements();
  if (focusables.length === 0) {
    event.preventDefault();
    dialogEl.value?.focus();
    return;
  }
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last?.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first?.focus();
  }
}

function onKeydown(event: KeyboardEvent) {
  if (!props.story) return;
  if (event.key === "Escape") {
    event.preventDefault();
    emit("close");
    return;
  }
  if (event.key === "Tab") {
    trapTab(event);
    return;
  }
  if (eventTargetUsesArrows(event.target)) return;
  if (event.key === "ArrowLeft" && hasPrevious.value) {
    event.preventDefault();
    emit("previous");
  } else if (event.key === "ArrowRight" && hasNext.value) {
    event.preventDefault();
    emit("next");
  }
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="story"
      :class="ui.positioner"
      role="presentation"
    >
      <div :class="ui.scrim" @click="emit('close')" />
      <div
        ref="dialogEl"
        :class="ui.dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Story"
        tabindex="-1"
        @click.stop
        @keydown="onKeydown"
      >
        <span :class="ui.grabber" aria-hidden="true"></span>
        <header :class="ui.head">
          <UserAvatar :name="authorLabel(story.author)" :jid="story.author ?? null" size="md" />
          <div :class="ui.who">
            <b>{{ authorLabel(story.author) }}</b>
            <span v-if="listedAgo(story)" :class="ui.caption">
              {{ listedAgo(story) }}
            </span>
          </div>
          <button
            type="button"
            :class="ui.close"
            aria-label="Close story"
            @click="emit('close')"
          >
            <X aria-hidden="true" />
          </button>
        </header>

        <div :class="ui.scroll">
          <div v-if="story.mediaUrl" :class="ui.media">
            <video
              v-if="isVideoStory(story)"
              :src="story.mediaUrl"
              controls
              playsinline
              referrerpolicy="no-referrer"
            ></video>
            <img
              v-else
              :src="story.mediaUrl"
              :alt="`Story media from ${authorLabel(story.author)}`"
              referrerpolicy="no-referrer"
            />
          </div>
          <p v-if="story.body" :class="ui.body">
            {{ story.body }}
          </p>

          <div :class="ui.reactions">
            <div :class="ui.quick">
              <button
                v-for="emoji in QUICK_REACTION_EMOJIS"
                :key="emoji"
                type="button"
                :class="ui.quickBtn"
                :aria-label="`React with ${emoji}`"
                :aria-pressed="reactionSummary?.mine.includes(emoji) ? 'true' : 'false'"
                @click="emit('react', emoji)"
              >
                {{ emoji }}
              </button>
            </div>
            <div v-if="reactionEntries.length > 0" :class="ui.summary">
              <span
                v-for="entry in reactionEntries"
                :key="entry.emoji"
                :class="ui.chip"
                :data-pressed="reactionSummary?.mine.includes(entry.emoji) ? '' : undefined"
                :title="entry.reactors.join(', ')"
              >
                <span>{{ entry.emoji }}</span>
                <span class="wd-chip__n">{{ entry.count }}</span>
              </span>
            </div>
          </div>
        </div>

        <footer :class="ui.foot">
          <button
            type="button"
            :class="navButton"
            :disabled="!hasPrevious"
            @click="emit('previous')"
          >
            <ChevronLeft aria-hidden="true" />
            Prev
          </button>
          <span v-if="storyIndex !== null" :class="ui.counter">
            {{ storyIndex + 1 }} / {{ storyCount }}
          </span>
          <button
            type="button"
            :class="navButton"
            :disabled="!hasNext"
            @click="emit('next')"
          >
            Next
            <ChevronRight aria-hidden="true" />
          </button>
        </footer>
      </div>
    </div>
  </Teleport>
</template>
