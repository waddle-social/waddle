<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import { ChevronLeft, MessageCircle, ScanText, UserRound } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ScrollDirectionSwitcher from "@/components/chat/ScrollDirectionSwitcher.vue";
import VersionFooter from "@/components/chat/VersionFooter.vue";
import type { ServerVersion } from "@/composables/useVersion";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import {
  buildActivityPublication,
  buildMoodPublication,
  buildTunePublication,
  describeActivityPublication,
  describeMoodPublication,
  describeTunePublication,
  formatPepKeyword,
  GENERAL_ACTIVITIES,
  MOOD_KINDS,
  normalizeActivitySpecific,
  type ActivityStatusDraft,
  type ActivityValidationErrors,
  type MoodStatusDraft,
  type TuneStatusDraft,
  type TuneValidationErrors,
} from "@/lib/status-publication-ui";

const props = defineProps<{
  session: WaddleSession;
  xmppClient?: BrowserXmppClient | null;
  webCommitSha?: string;
  serverVersion?: ServerVersion | null;
}>();

const emit = defineEmits<{
  close: [];
}>();

type FeedbackTone = "muted" | "success" | "error";

interface StatusFeedback {
  tone: FeedbackTone;
  message: string;
}

const fieldLabelClass = "type-section-label text-muted-foreground/75";
const fieldClass = "chat-field-control type-field disabled:opacity-50";
const textareaClass = "chat-field-control chat-textarea-control type-field disabled:opacity-50";
const primaryButtonClass = "chat-action-button chat-action-button--primary type-control disabled:opacity-40";
const secondaryButtonClass = "chat-action-button chat-action-button--secondary type-control disabled:opacity-40";

const moodOptions = MOOD_KINDS.map((kind) => ({
  value: kind,
  label: formatPepKeyword(kind),
}));

const activityOptions = GENERAL_ACTIVITIES.map((general) => ({
  value: general,
  label: formatPepKeyword(general),
}));

const moodDraft = reactive<MoodStatusDraft>({
  kind: "",
  text: "",
});

const activityDraft = reactive<ActivityStatusDraft>({
  general: "",
  specific: "",
  text: "",
});

const tuneDraft = reactive<TuneStatusDraft>({
  artist: "",
  title: "",
  source: "",
  length: "",
  rating: "",
  track: "",
  uri: "",
});

const moodBusy = ref(false);
const activityBusy = ref(false);
const tuneBusy = ref(false);

const moodFeedback = ref<StatusFeedback>({
  tone: "muted",
  message: "Share a quick sense of how the day feels right now.",
});
const activityFeedback = ref<StatusFeedback>({
  tone: "muted",
  message: "Publish a broad activity and add a more specific detail if it helps.",
});
const tuneFeedback = ref<StatusFeedback>({
  tone: "muted",
  message: "Share what is playing now, or only the parts that matter.",
});

const activityErrors = ref<ActivityValidationErrors>({});
const tuneErrors = ref<TuneValidationErrors>({});

const normalizedActivitySpecific = computed(() => normalizeActivitySpecific(activityDraft.specific));
const activitySpecificHint = computed(() => {
  const raw = activityDraft.specific.trim();
  if (!raw) return "Optional. If needed, spaces and punctuation become snake_case.";
  if (!normalizedActivitySpecific.value) return "Use letters or numbers so we can publish a clean identifier.";
  if (normalizedActivitySpecific.value !== raw.toLowerCase()) {
    return `Will publish as ${normalizedActivitySpecific.value}.`;
  }
  return "Looks good.";
});

function feedbackClass(tone: FeedbackTone): string {
  switch (tone) {
    case "success":
      return "text-emerald-700 dark:text-emerald-300";
    case "error":
      return "text-destructive";
    default:
      return "text-muted-foreground";
  }
}

function normalizeError(value: unknown): string {
  return value instanceof Error ? value.message : "Something went wrong.";
}

function resetMoodDraft() {
  moodDraft.kind = "";
  moodDraft.text = "";
}

function resetActivityDraft() {
  activityDraft.general = "";
  activityDraft.specific = "";
  activityDraft.text = "";
}

function resetTuneDraft() {
  tuneDraft.artist = "";
  tuneDraft.title = "";
  tuneDraft.source = "";
  tuneDraft.length = "";
  tuneDraft.rating = "";
  tuneDraft.track = "";
  tuneDraft.uri = "";
}

function applyActivityDraft() {
  activityDraft.specific = normalizedActivitySpecific.value;
  activityDraft.text = activityDraft.text.trim();
}

function applyTuneDraft(publication: ReturnType<typeof buildTunePublication>["publication"]) {
  if (!publication) return;
  tuneDraft.artist = publication.artist ?? "";
  tuneDraft.title = publication.title ?? "";
  tuneDraft.source = publication.source ?? "";
  tuneDraft.length = publication.length?.toString() ?? "";
  tuneDraft.rating = publication.rating?.toString() ?? "";
  tuneDraft.track = publication.track ?? "";
  tuneDraft.uri = publication.uri ?? "";
}

async function publishMoodStatus() {
  const { publication, error } = buildMoodPublication(moodDraft);
  if (!publication) {
    moodFeedback.value = {
      tone: "error",
      message: error ?? "Choose a mood to share.",
    };
    return;
  }
  if (!props.xmppClient) {
    moodFeedback.value = { tone: "error", message: "Reconnect before publishing your mood." };
    return;
  }

  moodBusy.value = true;
  try {
    await props.xmppClient.publishMood(publication);
    moodDraft.text = publication.text ?? "";
    moodFeedback.value = {
      tone: "success",
      message: describeMoodPublication(publication),
    };
  } catch (error) {
    moodFeedback.value = {
      tone: "error",
      message: normalizeError(error),
    };
  } finally {
    moodBusy.value = false;
  }
}

async function clearMoodStatus() {
  if (!props.xmppClient) {
    moodFeedback.value = { tone: "error", message: "Reconnect before clearing your mood." };
    return;
  }

  moodBusy.value = true;
  try {
    await props.xmppClient.retractMood();
    resetMoodDraft();
    moodFeedback.value = {
      tone: "success",
      message: "Mood cleared.",
    };
  } catch (error) {
    moodFeedback.value = {
      tone: "error",
      message: normalizeError(error),
    };
  } finally {
    moodBusy.value = false;
  }
}

async function publishActivityStatus() {
  const { publication, errors } = buildActivityPublication(activityDraft);
  activityErrors.value = errors;
  if (!publication) {
    activityFeedback.value = {
      tone: "error",
      message: "Check the activity fields and try again.",
    };
    return;
  }
  if (!props.xmppClient) {
    activityFeedback.value = { tone: "error", message: "Reconnect before publishing your activity." };
    return;
  }

  activityBusy.value = true;
  try {
    await props.xmppClient.publishActivity(publication);
    applyActivityDraft();
    activityFeedback.value = {
      tone: "success",
      message: describeActivityPublication(publication),
    };
  } catch (error) {
    activityFeedback.value = {
      tone: "error",
      message: normalizeError(error),
    };
  } finally {
    activityBusy.value = false;
  }
}

async function clearActivityStatus() {
  activityErrors.value = {};
  if (!props.xmppClient) {
    activityFeedback.value = { tone: "error", message: "Reconnect before clearing your activity." };
    return;
  }

  activityBusy.value = true;
  try {
    await props.xmppClient.retractActivity();
    resetActivityDraft();
    activityFeedback.value = {
      tone: "success",
      message: "Activity cleared.",
    };
  } catch (error) {
    activityFeedback.value = {
      tone: "error",
      message: normalizeError(error),
    };
  } finally {
    activityBusy.value = false;
  }
}

async function publishTuneStatus() {
  const { publication, errors } = buildTunePublication(tuneDraft);
  tuneErrors.value = errors;
  if (!publication) {
    tuneFeedback.value = {
      tone: "error",
      message: errors.form ?? "Check the tune fields and try again.",
    };
    return;
  }
  if (!props.xmppClient) {
    tuneFeedback.value = { tone: "error", message: "Reconnect before publishing your tune." };
    return;
  }

  tuneBusy.value = true;
  try {
    await props.xmppClient.publishTune(publication);
    applyTuneDraft(publication);
    tuneFeedback.value = {
      tone: "success",
      message: describeTunePublication(publication),
    };
  } catch (error) {
    tuneFeedback.value = {
      tone: "error",
      message: normalizeError(error),
    };
  } finally {
    tuneBusy.value = false;
  }
}

async function clearTuneStatus() {
  tuneErrors.value = {};
  if (!props.xmppClient) {
    tuneFeedback.value = { tone: "error", message: "Reconnect before clearing your tune." };
    return;
  }

  tuneBusy.value = true;
  try {
    await props.xmppClient.retractTune();
    resetTuneDraft();
    tuneFeedback.value = {
      tone: "success",
      message: "Tune cleared.",
    };
  } catch (error) {
    tuneFeedback.value = {
      tone: "error",
      message: normalizeError(error),
    };
  } finally {
    tuneBusy.value = false;
  }
}
</script>

<template>
  <div class="flex-1 min-w-0 min-h-0 overflow-auto bg-background">
    <div class="z-sticky sticky top-0 border-b border-border glass-surface">
      <div class="mx-auto flex h-14 max-w-4xl items-center gap-3 px-4 sm:px-6">
        <button
          class="chat-action-button chat-action-button--secondary type-control text-muted-foreground hover:text-foreground"
          type="button"
          @click="emit('close')"
        >
          <ChevronLeft class="h-4 w-4" />
          <span>Back</span>
        </button>
        <div class="min-w-0">
          <h1 class="type-chat-title">Settings</h1>
          <p class="type-caption text-muted-foreground">Share profile signals, tune reading flow, and check build details.</p>
        </div>
      </div>
    </div>

    <div class="mx-auto flex max-w-4xl flex-col gap-4 px-4 py-5 sm:px-6">
      <section class="chat-section-card glass-panel">
        <div class="flex items-center gap-3">
          <AppAvatar :name="session.username" :src="session.avatar_url" size="md" />
          <div class="min-w-0">
            <div class="type-section-label flex items-center gap-2 text-muted-foreground/70">
              <UserRound class="h-3.5 w-3.5" />
              <span>Signed in</span>
            </div>
            <h2 class="type-chat-title mt-1 truncate">{{ session.username }}</h2>
            <p class="type-caption text-muted-foreground">Reading preferences, profile signals, and build details for this device.</p>
          </div>
        </div>
      </section>

      <section class="chat-section-card glass-panel">
        <div class="flex items-start gap-3 pb-4">
          <div class="flex h-8 w-8 items-center justify-center rounded-lg bg-primary/10 text-primary">
            <MessageCircle class="h-4 w-4" />
          </div>
          <div class="chat-settings-copy max-w-2xl">
            <h2 class="type-pane-title">Profile signals</h2>
            <p class="type-caption text-muted-foreground">
              Publish lightweight XMPP profile signals for mood, activity, and tune. Keep them short, calm, and easy to scan.
            </p>
          </div>
        </div>

        <p
          v-if="!xmppClient"
          class="type-caption mb-4 rounded-lg border border-border/70 bg-muted/50 px-3 py-2 text-muted-foreground"
          role="status"
        >
          Reconnect before publishing or clearing profile signals.
        </p>

        <div class="divide-y divide-border/70">
          <form
            class="chat-settings-form pb-4"
            @submit.prevent="publishMoodStatus"
          >
            <div class="chat-settings-copy">
              <p class="type-section-label text-muted-foreground/70">XEP-0107</p>
              <h3 class="type-pane-title">Mood</h3>
              <p class="type-caption text-muted-foreground">
                Share how things feel right now, with an optional note for context.
              </p>
            </div>

            <div class="grid gap-3">
              <div class="chat-settings-field-row">
                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Mood</span>
                  <select v-model="moodDraft.kind" :class="fieldClass">
                    <option value="">Choose a mood</option>
                    <option
                      v-for="option in moodOptions"
                      :key="option.value"
                      :value="option.value"
                    >
                      {{ option.label }}
                    </option>
                  </select>
                </label>

                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Optional note</span>
                  <textarea
                    v-model="moodDraft.text"
                    :class="textareaClass"
                    maxlength="160"
                    placeholder="Settled in and ready to catch up."
                    rows="2"
                  />
                </label>
              </div>

              <div class="flex flex-wrap items-center gap-2">
                <button
                  type="submit"
                  :class="primaryButtonClass"
                  :disabled="moodBusy || !xmppClient"
                >
                  {{ moodBusy ? "Sharing…" : "Publish mood" }}
                </button>
                <button
                  type="button"
                  :class="secondaryButtonClass"
                  :disabled="moodBusy || !xmppClient"
                  @click="clearMoodStatus"
                >
                  Clear
                </button>
                <p class="type-caption" :class="feedbackClass(moodFeedback.tone)" aria-live="polite">
                  {{ moodFeedback.message }}
                </p>
              </div>
            </div>
          </form>

          <form
            class="chat-settings-form py-4"
            @submit.prevent="publishActivityStatus"
          >
            <div class="chat-settings-copy">
              <p class="type-section-label text-muted-foreground/70">XEP-0108</p>
              <h3 class="type-pane-title">Activity</h3>
              <p class="type-caption text-muted-foreground">
                Pick a broad category, then add a specific detail if you want a more precise signal.
              </p>
            </div>

            <div class="grid gap-3">
              <div class="grid gap-3 sm:grid-cols-2">
                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">General activity</span>
                  <select v-model="activityDraft.general" :class="fieldClass">
                    <option value="">Choose an activity</option>
                    <option
                      v-for="option in activityOptions"
                      :key="option.value"
                      :value="option.value"
                    >
                      {{ option.label }}
                    </option>
                  </select>
                  <p v-if="activityErrors.general" class="type-caption text-destructive">
                    {{ activityErrors.general }}
                  </p>
                </label>

                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Specific activity</span>
                  <input
                    v-model="activityDraft.specific"
                    :class="fieldClass"
                    maxlength="64"
                    placeholder="deep focus"
                    type="text"
                  />
                  <p
                    class="type-caption"
                    :class="activityErrors.specific ? 'text-destructive' : 'text-muted-foreground'"
                  >
                    {{ activityErrors.specific ?? activitySpecificHint }}
                  </p>
                </label>

                <label class="grid gap-1.5 sm:col-span-2">
                  <span :class="fieldLabelClass">Optional note</span>
                  <textarea
                    v-model="activityDraft.text"
                    :class="textareaClass"
                    maxlength="160"
                    placeholder="Heads down for the next hour."
                    rows="2"
                  />
                </label>
              </div>

              <div class="flex flex-wrap items-center gap-2">
                <button
                  type="submit"
                  :class="primaryButtonClass"
                  :disabled="activityBusy || !xmppClient"
                >
                  {{ activityBusy ? "Sharing…" : "Publish activity" }}
                </button>
                <button
                  type="button"
                  :class="secondaryButtonClass"
                  :disabled="activityBusy || !xmppClient"
                  @click="clearActivityStatus"
                >
                  Clear
                </button>
                <p class="type-caption" :class="feedbackClass(activityFeedback.tone)" aria-live="polite">
                  {{ activityFeedback.message }}
                </p>
              </div>
            </div>
          </form>

          <form
            class="chat-settings-form pt-4"
            @submit.prevent="publishTuneStatus"
          >
            <div class="chat-settings-copy">
              <p class="type-section-label text-muted-foreground/70">XEP-0118</p>
              <h3 class="type-pane-title">Tune</h3>
              <p class="type-caption text-muted-foreground">
                Share what is playing now. Any field is optional, but at least one detail is needed to publish.
              </p>
            </div>

            <div class="grid gap-3">
              <div class="grid gap-3 sm:grid-cols-2">
                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Title</span>
                  <input
                    v-model="tuneDraft.title"
                    :class="fieldClass"
                    maxlength="120"
                    placeholder="Come Together"
                    type="text"
                  />
                </label>

                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Artist</span>
                  <input
                    v-model="tuneDraft.artist"
                    :class="fieldClass"
                    maxlength="120"
                    placeholder="The Beatles"
                    type="text"
                  />
                </label>

                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Source</span>
                  <input
                    v-model="tuneDraft.source"
                    :class="fieldClass"
                    maxlength="120"
                    placeholder="Abbey Road"
                    type="text"
                  />
                </label>

                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Track</span>
                  <input
                    v-model="tuneDraft.track"
                    :class="fieldClass"
                    maxlength="40"
                    placeholder="1"
                    type="text"
                  />
                </label>

                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Length (seconds)</span>
                  <input
                    v-model="tuneDraft.length"
                    :class="fieldClass"
                    inputmode="numeric"
                    placeholder="259"
                    type="text"
                  />
                  <p
                    class="type-caption"
                    :class="tuneErrors.length ? 'text-destructive' : 'text-muted-foreground'"
                  >
                    {{ tuneErrors.length ?? "Optional. Use whole seconds." }}
                  </p>
                </label>

                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Rating</span>
                  <input
                    v-model="tuneDraft.rating"
                    :class="fieldClass"
                    inputmode="numeric"
                    placeholder="9"
                    type="text"
                  />
                  <p
                    class="type-caption"
                    :class="tuneErrors.rating ? 'text-destructive' : 'text-muted-foreground'"
                  >
                    {{ tuneErrors.rating ?? "Optional. Whole numbers from 1 to 10." }}
                  </p>
                </label>

                <label class="grid gap-1.5 sm:col-span-2">
                  <span :class="fieldLabelClass">URL or URI</span>
                  <input
                    v-model="tuneDraft.uri"
                    :class="fieldClass"
                    maxlength="240"
                    placeholder="https://example.com/track or spotify:track:…"
                    type="text"
                  />
                  <p
                    class="type-caption"
                    :class="tuneErrors.uri ? 'text-destructive' : 'text-muted-foreground'"
                  >
                    {{ tuneErrors.uri ?? "Optional. Use a full URL or URI." }}
                  </p>
                </label>
              </div>

              <div class="flex flex-wrap items-center gap-2">
                <button
                  type="submit"
                  :class="primaryButtonClass"
                  :disabled="tuneBusy || !xmppClient"
                >
                  {{ tuneBusy ? "Sharing…" : "Publish tune" }}
                </button>
                <button
                  type="button"
                  :class="secondaryButtonClass"
                  :disabled="tuneBusy || !xmppClient"
                  @click="clearTuneStatus"
                >
                  Clear
                </button>
                <p class="type-caption" :class="feedbackClass(tuneFeedback.tone)" aria-live="polite">
                  {{ tuneErrors.form ?? tuneFeedback.message }}
                </p>
              </div>
            </div>
          </form>
        </div>
      </section>

      <section class="chat-section-card glass-panel">
        <div class="flex items-start gap-3 pb-4">
          <div class="flex h-8 w-8 items-center justify-center rounded-lg bg-primary/10 text-primary">
            <ScanText class="h-4 w-4" />
          </div>
          <div class="chat-settings-copy">
            <h2 class="type-pane-title">Reading flow</h2>
            <p class="type-caption text-muted-foreground">
              Set whether the newest messages land at the bottom or the top.
            </p>
          </div>
        </div>
        <ScrollDirectionSwitcher />
      </section>

      <section class="chat-section-card glass-panel">
        <h2 class="type-pane-title">About this build</h2>
        <p class="type-caption pt-1 text-muted-foreground">
          Helpful version details when you need to compare client and server behavior.
        </p>
        <div class="pt-4">
          <VersionFooter
            :web-commit-sha="webCommitSha"
            :server-version="serverVersion"
            layout="detail"
          />
        </div>
      </section>
    </div>
  </div>
</template>
