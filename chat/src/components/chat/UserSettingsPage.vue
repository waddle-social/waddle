<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import spotError from "@waddle/design/assets/spot-error.svg?url";
import { css } from "styled-system/css";
import { seg, toggle } from "styled-system/recipes";
import { feedbackClass, settingsClasses as sc, type FeedbackTone } from "@/components/chat/settings-classes";
import { useTheme, type ThemeMode } from "@/preferences/theme";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import ReadReceiptSwitcher from "@/components/chat/ReadReceiptSwitcher.vue";
import ScrollDirectionSwitcher from "@/components/chat/ScrollDirectionSwitcher.vue";
import VCardEditor from "@/components/chat/VCardEditor.vue";
import VersionFooter from "@/components/chat/VersionFooter.vue";
import type { XmppServerVersion } from "@/shell/version";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { clearManualActivity, setManualActivity } from "@/presence/self-activity";
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
  messageSoundsEnabled?: boolean;
  webCommitSha?: string;
  serverVersion?: XmppServerVersion | null;
}>();

const emit = defineEmits<{
  close: [];
  "toggle-message-sounds": [];
}>();

interface StatusFeedback {
  tone: FeedbackTone;
  message: string;
}

const pageScrollClass = css({ flex: 1, minWidth: 0, minHeight: 0, overflowY: "auto", bg: "bg", color: "fg" });
const pageClass = css({
  width: "100%",
  maxWidth: "760px",
  boxSizing: "border-box",
  display: "flex",
  flexDirection: "column",
  gap: "3",
  padding: "14px 12px 48px",
  md: { gap: "5", padding: "32px 40px 64px" },
});
const titleClass = css({
  position: "absolute", width: "1px", height: "1px", overflow: "hidden", clip: "rect(0 0 0 0)", whiteSpace: "nowrap",
  md: { position: "static", width: "auto", height: "auto", overflow: "visible", clip: "auto", margin: 0, font: "700 30px/1.2 {fonts.display}", letterSpacing: "-0.01em" },
});
const offlineClass = css({
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  margin: 0,
  padding: "12px 14px",
  borderRadius: "control",
  bg: "color-mix(in srgb, {colors.presence.away} 12%, transparent)",
  border: "1px solid color-mix(in srgb, {colors.presence.away} 40%, transparent)",
  fontSize: "14px",
  "& img": { width: "36px", height: "36px", flex: "none" },
});
const themeGridClass = css({ display: "grid", gridTemplateColumns: "repeat(2, minmax(0, 1fr))", gap: "2.5" });
const themeOptionClass = css({
  display: "flex",
  flexDirection: "column",
  gap: "2",
  padding: "2.5",
  borderRadius: "control",
  border: "2px solid {colors.border.soft}",
  cursor: "pointer",
  "&:has(input:checked)": { borderColor: "action" },
  "&:has(input:focus-visible)": { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
});
const themeSwatchClass = css({
  display: "flex",
  alignItems: "flex-end",
  gap: "1",
  height: "56px",
  boxSizing: "border-box",
  padding: "2",
  borderRadius: "sm",
  bg: "bg",
  border: "1px solid {colors.border}",
  "& > :first-child": { width: "40%", height: "10px", borderRadius: "5px", bg: "surface.2", border: "1px solid {colors.border.soft}" },
  "& > :last-child": { width: "30%", height: "10px", borderRadius: "5px", bg: "action", marginLeft: "auto" },
});
const themeLabelClass = css({ display: "flex", alignItems: "center", gap: "2", fontSize: "14px", "& input": { accentColor: "action", margin: 0 } });
const segClass = seg();
const statusSegClass = [segClass, css({ alignSelf: "flex-start" })];
const rowClass = css({ display: "flex", alignItems: "center", gap: "3", flexWrap: "wrap" });
const rowCopyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5", fontSize: "15px" });
const switchClass = toggle();
const formClass = css({ display: "flex", flexDirection: "column", gap: "4" });

const { mode: themeMode, setTheme } = useTheme();
const themeOptions: ReadonlyArray<{ value: ThemeMode; label: string }> = [
  { value: "night", label: "Night" },
  { value: "daylight", label: "Daylight" },
];

type StatusTab = "mood" | "activity" | "tune";
const statusTabs: ReadonlyArray<{ value: StatusTab; label: string }> = [
  { value: "mood", label: "Mood" },
  { value: "activity", label: "Activity" },
  { value: "tune", label: "Listening" },
];
const statusTab = ref<StatusTab>("mood");

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
    setManualActivity(publication);
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
    clearManualActivity();
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
  <div :class="pageScrollClass">
    <main :class="pageClass">
      <h1 :class="titleClass">Settings</h1>

      <p v-if="!xmppClient" :class="offlineClass" role="status">
        <img :src="spotError" alt="" />
        Reconnect to edit your profile and status.
      </p>

      <VCardEditor :xmpp-client="xmppClient" :self-jid="session.jid">
        <template #avatar>
          <UserAvatar :name="session.username" :jid="session.jid" :fallback-src="session.avatar_url" size="lg" />
        </template>
      </VCardEditor>

      <section id="appearance" :class="sc.card" aria-labelledby="appearance-title">
        <h2 id="appearance-title" :class="sc.heading">Appearance</h2>
        <div role="radiogroup" aria-label="Theme" :class="themeGridClass">
          <label v-for="option in themeOptions" :key="option.value" :class="themeOptionClass">
            <span :class="themeSwatchClass" :data-theme="option.value" aria-hidden="true"><span /><span /></span>
            <span :class="themeLabelClass">
              <input
                type="radio"
                name="theme"
                :value="option.value"
                :checked="themeMode === option.value"
                @change="setTheme(option.value)"
              />
              {{ option.label }}
            </span>
          </label>
        </div>
      </section>

      <section id="status" :class="sc.card" aria-labelledby="status-title">
        <div :class="sc.cardHead">
          <h2 id="status-title" :class="sc.heading">Status</h2>
          <p :class="sc.caption">Shown to your contacts next to your name.</p>
        </div>
        <div role="group" aria-label="Status type" :class="statusSegClass">
          <button
            v-for="tab in statusTabs"
            :key="tab.value"
            type="button"
            :aria-pressed="statusTab === tab.value"
            @click="statusTab = tab.value"
          >
            {{ tab.label }}
          </button>
        </div>

        <form v-if="statusTab === 'mood'" :class="formClass" aria-label="Mood" @submit.prevent="publishMoodStatus">
          <div :class="sc.grid">
            <label :class="sc.label">
              Mood
              <select v-model="moodDraft.kind" :class="sc.input">
                <option value="">Choose a mood</option>
                <option v-for="option in moodOptions" :key="option.value" :value="option.value">
                  {{ option.label }}
                </option>
              </select>
            </label>
            <label :class="sc.label">
              Note (optional)
              <input
                v-model="moodDraft.text"
                :class="sc.input"
                maxlength="160"
                placeholder="Settled in and ready to catch up."
                type="text"
              />
            </label>
          </div>
          <div :class="sc.actions">
            <button type="submit" :class="sc.primary" :disabled="moodBusy || !xmppClient">
              {{ moodBusy ? "Sharing…" : "Set mood" }}
            </button>
            <button type="button" :class="sc.secondary" :disabled="moodBusy || !xmppClient" @click="clearMoodStatus">
              Clear
            </button>
            <p :class="feedbackClass(moodFeedback.tone)" aria-live="polite">{{ moodFeedback.message }}</p>
          </div>
        </form>

        <form v-else-if="statusTab === 'activity'" :class="formClass" aria-label="Activity" @submit.prevent="publishActivityStatus">
          <div :class="sc.grid">
            <label :class="sc.label">
              Activity
              <select v-model="activityDraft.general" :class="sc.input" :aria-invalid="activityErrors.general ? 'true' : undefined">
                <option value="">Choose an activity</option>
                <option v-for="option in activityOptions" :key="option.value" :value="option.value">
                  {{ option.label }}
                </option>
              </select>
              <small v-if="activityErrors.general" :class="sc.hintError">{{ activityErrors.general }}</small>
            </label>
            <label :class="sc.label">
              Specific activity (optional)
              <input
                v-model="activityDraft.specific"
                :class="sc.input"
                :aria-invalid="activityErrors.specific ? 'true' : undefined"
                maxlength="64"
                placeholder="deep focus"
                type="text"
              />
              <small :class="activityErrors.specific ? sc.hintError : sc.hint">
                {{ activityErrors.specific ?? activitySpecificHint }}
              </small>
            </label>
            <label :class="[sc.label, sc.wide]">
              Note (optional)
              <input
                v-model="activityDraft.text"
                :class="sc.input"
                maxlength="160"
                placeholder="Heads down for the next hour."
                type="text"
              />
            </label>
          </div>
          <div :class="sc.actions">
            <button type="submit" :class="sc.primary" :disabled="activityBusy || !xmppClient">
              {{ activityBusy ? "Sharing…" : "Set activity" }}
            </button>
            <button type="button" :class="sc.secondary" :disabled="activityBusy || !xmppClient" @click="clearActivityStatus">
              Clear
            </button>
            <p :class="feedbackClass(activityFeedback.tone)" aria-live="polite">{{ activityFeedback.message }}</p>
          </div>
        </form>

        <form v-else :class="formClass" aria-label="Listening" @submit.prevent="publishTuneStatus">
          <div :class="sc.grid">
            <label :class="sc.label">
              Title
              <input v-model="tuneDraft.title" :class="sc.input" maxlength="120" placeholder="Come Together" type="text" />
            </label>
            <label :class="sc.label">
              Artist
              <input v-model="tuneDraft.artist" :class="sc.input" maxlength="120" placeholder="The Beatles" type="text" />
            </label>
            <label :class="sc.label">
              Album or source (optional)
              <input v-model="tuneDraft.source" :class="sc.input" maxlength="120" placeholder="Abbey Road" type="text" />
            </label>
            <label :class="sc.label">
              Track number (optional)
              <input v-model="tuneDraft.track" :class="sc.input" maxlength="40" placeholder="1" type="text" />
            </label>
            <label :class="sc.label">
              Length in seconds (optional)
              <input
                v-model="tuneDraft.length"
                :class="sc.input"
                :aria-invalid="tuneErrors.length ? 'true' : undefined"
                inputmode="numeric"
                placeholder="259"
                type="text"
              />
              <small :class="tuneErrors.length ? sc.hintError : sc.hint">{{ tuneErrors.length ?? "Whole seconds." }}</small>
            </label>
            <label :class="sc.label">
              Rating (optional)
              <input
                v-model="tuneDraft.rating"
                :class="sc.input"
                :aria-invalid="tuneErrors.rating ? 'true' : undefined"
                inputmode="numeric"
                placeholder="9"
                type="text"
              />
              <small :class="tuneErrors.rating ? sc.hintError : sc.hint">{{ tuneErrors.rating ?? "1 to 10." }}</small>
            </label>
            <label :class="[sc.label, sc.wide]">
              Link (optional)
              <input
                v-model="tuneDraft.uri"
                :class="sc.input"
                :aria-invalid="tuneErrors.uri ? 'true' : undefined"
                maxlength="240"
                placeholder="https://… or spotify:track:…"
                type="text"
              />
              <small :class="tuneErrors.uri ? sc.hintError : sc.hint">{{ tuneErrors.uri ?? "A full URL or URI." }}</small>
            </label>
          </div>
          <div :class="sc.actions">
            <button type="submit" :class="sc.primary" :disabled="tuneBusy || !xmppClient">
              {{ tuneBusy ? "Sharing…" : "Set tune" }}
            </button>
            <button type="button" :class="sc.secondary" :disabled="tuneBusy || !xmppClient" @click="clearTuneStatus">
              Clear
            </button>
            <p :class="feedbackClass(tuneErrors.form ? 'error' : tuneFeedback.tone)" aria-live="polite">
              {{ tuneErrors.form ?? tuneFeedback.message }}
            </p>
          </div>
        </form>
      </section>

      <section id="messages" :class="sc.card" aria-labelledby="messages-title">
        <h2 id="messages-title" :class="sc.heading">Messages</h2>
        <ScrollDirectionSwitcher />
        <div :class="rowClass">
          <span :class="rowCopyClass">
            <span id="message-sounds-title">Message sounds</span>
            <span id="message-sounds-description" :class="sc.caption">
              {{ messageSoundsEnabled !== false ? "In-app sounds are on for this device." : "In-app sounds are off for this device." }}
            </span>
          </span>
          <button
            type="button"
            role="switch"
            :class="switchClass"
            :aria-checked="messageSoundsEnabled !== false"
            aria-labelledby="message-sounds-title"
            aria-describedby="message-sounds-description"
            @click="emit('toggle-message-sounds')"
          />
        </div>
        <ReadReceiptSwitcher />
      </section>

      <section id="about" :class="sc.card" aria-labelledby="about-title">
        <h2 id="about-title" :class="sc.heading">About this build</h2>
        <VersionFooter :web-commit-sha="webCommitSha" :server-version="serverVersion" layout="detail" />
      </section>
    </main>
  </div>
</template>
