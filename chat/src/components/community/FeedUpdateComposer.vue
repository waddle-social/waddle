<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
import { computed, reactive, ref, watch, type Component } from "vue";
import {
  Briefcase,
  Camera,
  IdCard,
  MessageSquareText,
  Music,
  RefreshCw,
  Send,
  Smile,
} from "lucide-vue-next";
import StoryComposer from "@/components/community/StoryComposer.vue";
import { connectionStore } from "@/lib/connection-store";
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
import type { FeedPostInput, StoryPostInput } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { ActivityPublication, MoodPublication, TunePublication } from "@/lib/xmpp/pep-types";
import {
  draftFromProfile,
  profileFromDraft,
  profilesEqual,
  type VCard4Draft,
  type VCard4Profile,
} from "@/lib/xmpp/vcard4-types";
import type { FeedSurfaceComposerMode } from "@/shell/state";
import { css, cx } from "styled-system/css";
import { btn, field, floe, input, seg } from "styled-system/recipes";

interface FeedUpdateComposerProps {
  selfJid: string | null;
  isPosting: boolean;
  isStoryPosting: boolean;
  initialMode?: FeedSurfaceComposerMode;
  publishPost?: (input: FeedPostInput) => Promise<unknown>;
  publishStory?: (input: StoryPostInput) => Promise<unknown>;
  publishMood?: (input: MoodPublication) => Promise<void>;
  publishActivity?: (input: ActivityPublication) => Promise<void>;
  publishTune?: (input: TunePublication) => Promise<void>;
  fetchProfile?: () => Promise<VCard4Profile | null>;
  publishProfile?: (input: VCard4Profile) => Promise<void>;
}

const props = withDefaults(defineProps<FeedUpdateComposerProps>(), {
  initialMode: "post",
});

type ComposerMode = "post" | "story" | "mood" | "activity" | "tune" | "profile";
type FeedbackTone = "muted" | "success" | "error";

interface ComposerFeedback {
  tone: FeedbackTone;
  message: string;
}

const COMPOSER_MODES: readonly { id: ComposerMode; label: string; icon: Component }[] = [
  { id: "post", label: "Post", icon: MessageSquareText },
  { id: "story", label: "Story", icon: Camera },
  { id: "mood", label: "Mood", icon: Smile },
  { id: "activity", label: "Activity", icon: Briefcase },
  { id: "tune", label: "Tune", icon: Music },
  { id: "profile", label: "Profile", icon: IdCard },
];

const ui = {
  root: cx(floe(), css({ "--pad": "16px", gap: "3", _focusWithin: { borderColor: "border.strong" } })),
  modes: cx(seg(), css({ alignSelf: "flex-start", maxWidth: "100%", overflowX: "auto", flex: "none" })),
  form: css({ display: "flex", flexDirection: "column", gap: "3" }),
  grid: css({ display: "grid", gap: "3", gridTemplateColumns: { base: "minmax(0, 1fr)", sm: "repeat(2, minmax(0, 1fr))" } }),
  gridLead: css({ display: "grid", gap: "3", gridTemplateColumns: { base: "minmax(0, 1fr)", sm: "minmax(0, 12rem) minmax(0, 1fr)" } }),
  postBody: css({
    resize: "none",
    minHeight: "4rem",
    border: 0,
    bg: "transparent",
    color: "fg",
    font: "15px/1.5 {fonts.body}",
    padding: "4px 6px",
    borderRadius: "sm",
    _placeholder: { color: "fg.soft" },
    _focusVisible: { outlineOffset: "0" },
    _disabled: { opacity: 0.5 },
  }),
  actions: css({ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "2" }),
  end: css({ display: "flex", justifyContent: "flex-end" }),
  fieldError: css({ textStyle: "caption", color: "danger.text" }),
};
const fieldClass = cx(field(), css({ "& > span:first-child": { textStyle: "label", color: "fg" } }));
const inputClass = input();
const textareaClass = cx(input(), css({ height: "auto", minHeight: "4rem", py: "2.5", resize: "vertical", font: "14px/1.5 {fonts.body}" }));
const primaryButtonClass = btn({ variant: "primary" });
const quietButtonClass = btn();
const counterClass = { base: css({ textStyle: "caption", color: "fg.muted" }), over: css({ textStyle: "caption", color: "danger.text" }) };
const FEEDBACK_CLASS: Record<FeedbackTone, string> = {
  muted: css({ textStyle: "caption", color: "fg.muted" }),
  success: css({ textStyle: "caption", color: "success.text" }),
  error: css({ textStyle: "caption", color: "danger.text" }),
};
const COMPOSER_MAX = 500;

const composerMode = ref<ComposerMode>(props.initialMode);
const composerBody = ref("");
const composerFeedback = ref<ComposerFeedback | null>(null);
const postBusy = ref(false);
const storyComposerError = ref<string | null>(null);
const storyComposerBusy = ref(false);
const storyComposerKey = ref(0);
const pepBusy = ref<ComposerMode | null>(null);

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
const activityErrors = ref<ActivityValidationErrors>({});
const tuneErrors = ref<TuneValidationErrors>({});
const profileDraft = reactive<VCard4Draft>(draftFromProfile(null));
const profilePersisted = ref<VCard4Profile>({});
const profileLoaded = ref(false);
const profileLoading = ref(false);
let profileLoadRequestId = 0;
const normalizedActivitySpecific = computed(() => normalizeActivitySpecific(activityDraft.specific));

const canSubmitPost = computed(() => {
  return !props.isPosting && !postBusy.value && composerBody.value.trim().length > 0;
});
const composerOver = computed(() => composerBody.value.length > COMPOSER_MAX);

watch(composerMode, (mode) => {
  composerFeedback.value = null;
  storyComposerError.value = null;
  if (mode === "profile" && !profileLoaded.value && !profileLoading.value) {
    void loadProfileDraft();
  }
});

watch(() => props.selfJid, () => {
  profileLoadRequestId += 1;
  profileLoaded.value = false;
  profileLoading.value = false;
  profilePersisted.value = {};
  applyProfileDraft({});
  if (composerMode.value === "profile" && props.selfJid) {
    void loadProfileDraft();
  }
});

watch(() => props.initialMode, (mode) => {
  composerMode.value = mode;
});

function errorMessage(value: unknown, fallback = "Something went wrong."): string {
  return value instanceof Error ? value.message : fallback;
}

function setComposerFeedback(mode: ComposerMode, tone: FeedbackTone, message: string) {
  if (composerMode.value !== mode) return;
  composerFeedback.value = { tone, message };
}

function applyProfileDraft(profile: VCard4Profile) {
  const next = draftFromProfile(profile);
  profileDraft.fullName = next.fullName;
  profileDraft.nickname = next.nickname;
  profileDraft.pronouns = next.pronouns;
  profileDraft.note = next.note;
  profileDraft.url = next.url;
  profileDraft.photoUri = next.photoUri;
}

async function loadProfileDraft() {
  if (!props.fetchProfile) {
    profileLoaded.value = false;
    setComposerFeedback("profile", "error", "Reconnect before loading your profile.");
    return;
  }
  const requestId = ++profileLoadRequestId;
  const selfJid = props.selfJid;
  profileLoading.value = true;
  try {
    const profile = await props.fetchProfile();
    if (requestId !== profileLoadRequestId || selfJid !== props.selfJid) return;
    const next = profile ?? {};
    profilePersisted.value = next;
    applyProfileDraft(next);
    profileLoaded.value = true;
    setComposerFeedback("profile", "muted", profile ? "Profile loaded." : "No profile saved yet.");
  } catch (err) {
    if (requestId !== profileLoadRequestId || selfJid !== props.selfJid) return;
    profileLoaded.value = false;
    setComposerFeedback("profile", "error", `Couldn't load profile: ${errorMessage(err)}`);
  } finally {
    if (requestId === profileLoadRequestId) {
      profileLoading.value = false;
    }
  }
}

async function submitPost() {
  const body = composerBody.value.trim();
  if (!body || composerOver.value) return;
  if (!props.publishPost) {
    setComposerFeedback("post", "error", "Reconnect before publishing your post.");
    return;
  }
  postBusy.value = true;
  try {
    await props.publishPost({ body, ...(props.selfJid ? { author: barePeerJid(props.selfJid) } : {}) });
    composerBody.value = "";
    setComposerFeedback("post", "success", "Post shared.");
  } catch (err) {
    setComposerFeedback("post", "error", errorMessage(err, "Couldn't publish your post."));
  } finally {
    postBusy.value = false;
  }
}

async function handleStorySubmit(payload: { body?: string; file: Blob; mediaKind: "image" | "video" }) {
  const client = connectionStore.client;
  if (!client || !props.publishStory) {
    storyComposerError.value = "Not connected.";
    return;
  }
  storyComposerError.value = null;
  storyComposerBusy.value = true;
  try {
    const uploaded = await client.uploadStoryMedia(payload.file);
    await props.publishStory({
      ...(payload.body ? { body: payload.body } : {}),
      mediaUrl: uploaded.url,
      mediaType: uploaded.contentType,
      ...(props.selfJid ? { author: barePeerJid(props.selfJid) } : {}),
    });
    storyComposerKey.value += 1;
    setComposerFeedback("story", "success", "Story shared.");
  } catch (err) {
    if (composerMode.value === "story") {
      storyComposerError.value = errorMessage(err, "Couldn't upload - please try again.");
    }
  } finally {
    storyComposerBusy.value = false;
  }
}

async function publishMood() {
  const { publication, error } = buildMoodPublication(moodDraft);
  if (!publication) {
    setComposerFeedback("mood", "error", error ?? "Choose a mood to share.");
    return;
  }
  if (!props.publishMood) {
    setComposerFeedback("mood", "error", "Reconnect before publishing your mood.");
    return;
  }
  pepBusy.value = "mood";
  try {
    await props.publishMood(publication);
    moodDraft.text = publication.text ?? "";
    setComposerFeedback("mood", "success", describeMoodPublication(publication));
  } catch (err) {
    setComposerFeedback("mood", "error", errorMessage(err, "Couldn't publish your mood."));
  } finally {
    pepBusy.value = null;
  }
}

async function publishActivity() {
  const { publication, errors } = buildActivityPublication(activityDraft);
  activityErrors.value = errors;
  if (!publication) {
    setComposerFeedback("activity", "error", "Check the activity fields and try again.");
    return;
  }
  if (!props.publishActivity) {
    setComposerFeedback("activity", "error", "Reconnect before publishing your activity.");
    return;
  }
  pepBusy.value = "activity";
  try {
    await props.publishActivity(publication);
    activityDraft.specific = normalizedActivitySpecific.value;
    activityDraft.text = publication.text ?? "";
    setComposerFeedback("activity", "success", describeActivityPublication(publication));
  } catch (err) {
    setComposerFeedback("activity", "error", errorMessage(err, "Couldn't publish your activity."));
  } finally {
    pepBusy.value = null;
  }
}

function applyTuneDraft(publication: TunePublication) {
  tuneDraft.artist = publication.artist ?? "";
  tuneDraft.title = publication.title ?? "";
  tuneDraft.source = publication.source ?? "";
  tuneDraft.length = publication.length?.toString() ?? "";
  tuneDraft.rating = publication.rating?.toString() ?? "";
  tuneDraft.track = publication.track ?? "";
  tuneDraft.uri = publication.uri ?? "";
}

async function publishTune() {
  const { publication, errors } = buildTunePublication(tuneDraft);
  tuneErrors.value = errors;
  if (!publication) {
    setComposerFeedback("tune", "error", errors.form ?? "Check the tune fields and try again.");
    return;
  }
  if (!props.publishTune) {
    setComposerFeedback("tune", "error", "Reconnect before publishing your tune.");
    return;
  }
  pepBusy.value = "tune";
  try {
    await props.publishTune(publication);
    applyTuneDraft(publication);
    setComposerFeedback("tune", "success", describeTunePublication(publication));
  } catch (err) {
    setComposerFeedback("tune", "error", errorMessage(err, "Couldn't publish your tune."));
  } finally {
    pepBusy.value = null;
  }
}

async function publishProfile() {
  if (!props.publishProfile) {
    setComposerFeedback("profile", "error", "Reconnect before publishing your profile.");
    return;
  }
  if (!profileLoaded.value) {
    setComposerFeedback("profile", "error", "Load your saved profile before publishing changes.");
    return;
  }
  const next = profileFromDraft(profileDraft);
  if (profilesEqual(next, profilePersisted.value)) {
    setComposerFeedback("profile", "muted", "Nothing to publish.");
    return;
  }
  const previous = profilePersisted.value;
  profilePersisted.value = next;
  pepBusy.value = "profile";
  try {
    await props.publishProfile(next);
    profileLoaded.value = true;
    setComposerFeedback("profile", "success", "Profile published.");
  } catch (err) {
    profilePersisted.value = previous;
    applyProfileDraft(previous);
    setComposerFeedback("profile", "error", errorMessage(err, "Couldn't publish your profile."));
  } finally {
    pepBusy.value = null;
  }
}
</script>

<template>
  <section :class="ui.root" aria-label="Create feed update">
    <div :class="ui.modes" role="tablist" aria-label="Feed update type">
      <button
        v-for="mode in COMPOSER_MODES"
        :key="mode.id"
        type="button"
        role="tab"
        :aria-selected="composerMode === mode.id ? 'true' : 'false'"
        @click="composerMode = mode.id"
      >
        <component :is="mode.icon" aria-hidden="true" />
        {{ mode.label }}
      </button>
    </div>

    <form v-if="composerMode === 'post'" :class="ui.form" @submit.prevent="submitPost">
      <textarea
        v-model="composerBody"
        :class="ui.postBody"
        rows="2"
        placeholder="Share something with the community"
        :disabled="isPosting || postBusy"
        aria-label="Feed post body"
      />
      <div :class="ui.actions">
        <span :class="composerOver ? counterClass.over : counterClass.base">
          {{ composerBody.length }} / {{ COMPOSER_MAX }}
        </span>
        <button
          type="submit"
          :class="primaryButtonClass"
          :disabled="!canSubmitPost || composerOver"
        >
          <Send aria-hidden="true" />
          {{ isPosting || postBusy ? "Posting..." : "Post" }}
        </button>
      </div>
    </form>

    <StoryComposer
      v-else-if="composerMode === 'story'"
      :key="storyComposerKey"
      embedded
      :busy="storyComposerBusy || isStoryPosting"
      @submit="handleStorySubmit"
      @cancel="storyComposerError = null"
    />

    <form v-else-if="composerMode === 'mood'" :class="ui.form" @submit.prevent="publishMood">
      <div :class="ui.gridLead">
        <label :class="fieldClass">
          <span>Mood</span>
          <AppSelect
            v-model="moodDraft.kind"
            :options="MOOD_KINDS.map((kind) => ({ value: kind, label: formatPepKeyword(kind) }))"
            placeholder="Choose mood"
            :class="inputClass"
            :disabled="pepBusy === 'mood'"
          />
        </label>
        <label :class="fieldClass">
          <span>Text</span>
          <input
            v-model="moodDraft.text"
            :class="inputClass"
            :disabled="pepBusy === 'mood'"
            maxlength="180"
            placeholder="Optional note"
            type="text"
          />
        </label>
      </div>
      <div :class="ui.end">
        <button
          type="submit"
          :class="primaryButtonClass"
          :disabled="pepBusy !== null"
        >
          <Smile aria-hidden="true" />
          {{ pepBusy === 'mood' ? "Publishing..." : "Publish mood" }}
        </button>
      </div>
    </form>

    <form v-else-if="composerMode === 'activity'" :class="ui.form" @submit.prevent="publishActivity">
      <div :class="ui.gridLead">
        <label :class="fieldClass">
          <span>Activity</span>
          <AppSelect
            v-model="activityDraft.general"
            :options="GENERAL_ACTIVITIES.map((general) => ({ value: general, label: formatPepKeyword(general) }))"
            placeholder="Choose activity"
            :class="inputClass"
            :disabled="pepBusy === 'activity'"
          />
          <span v-if="activityErrors.general" :class="ui.fieldError">{{ activityErrors.general }}</span>
        </label>
        <label :class="fieldClass">
          <span>Specific</span>
          <input
            v-model="activityDraft.specific"
            :class="inputClass"
            :disabled="pepBusy === 'activity'"
            maxlength="80"
            placeholder="Optional detail"
            type="text"
          />
          <span v-if="activityErrors.specific" :class="ui.fieldError">{{ activityErrors.specific }}</span>
        </label>
      </div>
      <label :class="fieldClass">
        <span>Text</span>
        <input
          v-model="activityDraft.text"
          :class="inputClass"
          :disabled="pepBusy === 'activity'"
          maxlength="180"
          placeholder="Optional note"
          type="text"
        />
      </label>
      <div :class="ui.end">
        <button
          type="submit"
          :class="primaryButtonClass"
          :disabled="pepBusy !== null"
        >
          <Briefcase aria-hidden="true" />
          {{ pepBusy === 'activity' ? "Publishing..." : "Publish activity" }}
        </button>
      </div>
    </form>

    <form v-else-if="composerMode === 'tune'" :class="ui.form" @submit.prevent="publishTune">
      <div :class="ui.grid">
        <label :class="fieldClass">
          <span>Title</span>
          <input
            v-model="tuneDraft.title"
            :class="inputClass"
            :disabled="pepBusy === 'tune'"
            maxlength="160"
            type="text"
          />
        </label>
        <label :class="fieldClass">
          <span>Artist</span>
          <input
            v-model="tuneDraft.artist"
            :class="inputClass"
            :disabled="pepBusy === 'tune'"
            maxlength="160"
            type="text"
          />
        </label>
        <label :class="fieldClass">
          <span>Source</span>
          <input
            v-model="tuneDraft.source"
            :class="inputClass"
            :disabled="pepBusy === 'tune'"
            maxlength="160"
            type="text"
          />
        </label>
        <label :class="fieldClass">
          <span>URI</span>
          <input
            v-model="tuneDraft.uri"
            :class="inputClass"
            :disabled="pepBusy === 'tune'"
            maxlength="240"
            type="text"
          />
          <span v-if="tuneErrors.uri" :class="ui.fieldError">{{ tuneErrors.uri }}</span>
        </label>
        <label :class="fieldClass">
          <span>Length</span>
          <input
            v-model="tuneDraft.length"
            :class="inputClass"
            :disabled="pepBusy === 'tune'"
            inputmode="numeric"
            type="text"
          />
          <span v-if="tuneErrors.length" :class="ui.fieldError">{{ tuneErrors.length }}</span>
        </label>
        <label :class="fieldClass">
          <span>Rating</span>
          <input
            v-model="tuneDraft.rating"
            :class="inputClass"
            :disabled="pepBusy === 'tune'"
            inputmode="numeric"
            type="text"
          />
          <span v-if="tuneErrors.rating" :class="ui.fieldError">{{ tuneErrors.rating }}</span>
        </label>
      </div>
      <p v-if="tuneErrors.form" :class="ui.fieldError">{{ tuneErrors.form }}</p>
      <div :class="ui.end">
        <button
          type="submit"
          :class="primaryButtonClass"
          :disabled="pepBusy !== null"
        >
          <Music aria-hidden="true" />
          {{ pepBusy === 'tune' ? "Publishing..." : "Publish tune" }}
        </button>
      </div>
    </form>

    <form v-else :class="ui.form" @submit.prevent="publishProfile">
      <div :class="ui.grid">
        <label :class="fieldClass">
          <span>Full name</span>
          <input
            v-model="profileDraft.fullName"
            :class="inputClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="160"
            type="text"
          />
        </label>
        <label :class="fieldClass">
          <span>Nickname</span>
          <input
            v-model="profileDraft.nickname"
            :class="inputClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="80"
            type="text"
          />
        </label>
        <label :class="fieldClass">
          <span>Pronouns</span>
          <input
            v-model="profileDraft.pronouns"
            :class="inputClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="48"
            type="text"
          />
        </label>
        <label :class="fieldClass">
          <span>URL</span>
          <input
            v-model="profileDraft.url"
            :class="inputClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="240"
            type="url"
          />
        </label>
      </div>
      <label :class="fieldClass">
        <span>Bio</span>
        <textarea
          v-model="profileDraft.note"
          :class="textareaClass"
          :disabled="profileLoading || pepBusy === 'profile'"
          maxlength="500"
        />
      </label>
      <div :class="ui.actions">
        <button
          type="button"
          :class="quietButtonClass"
          :disabled="profileLoading || pepBusy !== null"
          @click="loadProfileDraft"
        >
          <RefreshCw aria-hidden="true" />
          {{ profileLoading ? "Loading..." : "Reload" }}
        </button>
        <button
          type="submit"
          :class="primaryButtonClass"
          :disabled="profileLoading || pepBusy !== null || !profileLoaded"
        >
          <IdCard aria-hidden="true" />
          {{ pepBusy === 'profile' ? "Publishing..." : "Publish profile" }}
        </button>
      </div>
    </form>

    <p v-if="composerFeedback" :class="FEEDBACK_CLASS[composerFeedback.tone]" role="status">
      {{ composerFeedback.message }}
    </p>
    <p v-if="composerMode === 'story' && storyComposerError" :class="ui.fieldError" role="status">
      {{ storyComposerError }}
    </p>
  </section>
</template>
