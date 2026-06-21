<script setup lang="ts">
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
import AppAvatar from "@/components/ui/AppAvatar.vue";
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
import type { ActivityPublication, MoodPublication, TunePublication } from "@/lib/xmpp/pep-types";
import {
  draftFromProfile,
  profileFromDraft,
  profilesEqual,
  type VCard4Draft,
  type VCard4Profile,
} from "@/lib/xmpp/vcard4-types";
import type { FeedSurfaceComposerMode } from "@/shell/state";

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

const fieldLabelClass = "type-section-label text-muted-foreground/75";
const fieldClass = "rounded-md border border-input bg-background px-3 py-2 text-sm text-foreground shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-50";
const textareaClass = `${fieldClass} min-h-[4rem] resize-y`;
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

function authorLabel(author: string | undefined): string {
  if (!author) return "Anonymous";
  return author.split("@")[0] ?? author;
}

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
    await props.publishPost({ body, ...(props.selfJid ? { author: props.selfJid.split("/")[0] } : {}) });
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
      ...(props.selfJid ? { author: props.selfJid.split("/")[0] } : {}),
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
  <section
    class="grid gap-3 rounded-xl border border-border bg-card p-4 shadow-sm focus-within:ring-1 focus-within:ring-ring/40"
    aria-label="Create feed update"
  >
    <div class="flex items-center gap-3">
      <AppAvatar :name="authorLabel(selfJid ?? '')" :src="null" size="md" />
      <div class="min-w-0 flex-1">
        <div class="flex flex-wrap items-center gap-1" role="tablist" aria-label="Feed update type">
          <button
            v-for="mode in COMPOSER_MODES"
            :key="mode.id"
            type="button"
            role="tab"
            class="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1.5 text-xs transition-colors"
            :class="composerMode === mode.id ? 'border-primary bg-primary/10 text-primary' : 'border-input text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
            :aria-selected="composerMode === mode.id ? 'true' : 'false'"
            @click="composerMode = mode.id"
          >
            <component :is="mode.icon" class="h-3.5 w-3.5" aria-hidden="true" />
            {{ mode.label }}
          </button>
        </div>
      </div>
    </div>

    <form v-if="composerMode === 'post'" class="grid gap-3" @submit.prevent="submitPost">
      <textarea
        v-model="composerBody"
        :class="textareaClass"
        placeholder="Share something with the community..."
        :disabled="isPosting || postBusy"
        aria-label="Feed post body"
      />
      <div class="flex items-center justify-between gap-2">
        <span class="type-caption" :class="composerOver ? 'text-destructive' : 'text-muted-foreground'">
          {{ composerBody.length }} / {{ COMPOSER_MAX }}
        </span>
        <button
          type="submit"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3.5 py-1.5 text-xs font-medium text-primary-foreground shadow-sm transition-opacity hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="!canSubmitPost || composerOver"
        >
          <Send class="h-3.5 w-3.5" aria-hidden="true" />
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

    <form v-else-if="composerMode === 'mood'" class="grid gap-3" @submit.prevent="publishMood">
      <div class="grid gap-3 sm:grid-cols-[minmax(0,12rem)_minmax(0,1fr)]">
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Mood</span>
          <select v-model="moodDraft.kind" :class="fieldClass" :disabled="pepBusy === 'mood'">
            <option value="">Choose mood</option>
            <option v-for="kind in MOOD_KINDS" :key="kind" :value="kind">
              {{ formatPepKeyword(kind) }}
            </option>
          </select>
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Text</span>
          <input
            v-model="moodDraft.text"
            :class="fieldClass"
            :disabled="pepBusy === 'mood'"
            maxlength="180"
            placeholder="Optional note"
            type="text"
          />
        </label>
      </div>
      <div class="flex justify-end">
        <button
          type="submit"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3.5 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="pepBusy !== null"
        >
          <Smile class="h-3.5 w-3.5" aria-hidden="true" />
          {{ pepBusy === 'mood' ? "Publishing..." : "Publish mood" }}
        </button>
      </div>
    </form>

    <form v-else-if="composerMode === 'activity'" class="grid gap-3" @submit.prevent="publishActivity">
      <div class="grid gap-3 sm:grid-cols-[minmax(0,12rem)_minmax(0,1fr)]">
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Activity</span>
          <select v-model="activityDraft.general" :class="fieldClass" :disabled="pepBusy === 'activity'">
            <option value="">Choose activity</option>
            <option v-for="general in GENERAL_ACTIVITIES" :key="general" :value="general">
              {{ formatPepKeyword(general) }}
            </option>
          </select>
          <span v-if="activityErrors.general" class="type-caption text-destructive">{{ activityErrors.general }}</span>
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Specific</span>
          <input
            v-model="activityDraft.specific"
            :class="fieldClass"
            :disabled="pepBusy === 'activity'"
            maxlength="80"
            placeholder="Optional detail"
            type="text"
          />
          <span v-if="activityErrors.specific" class="type-caption text-destructive">{{ activityErrors.specific }}</span>
        </label>
      </div>
      <label class="grid gap-1.5">
        <span :class="fieldLabelClass">Text</span>
        <input
          v-model="activityDraft.text"
          :class="fieldClass"
          :disabled="pepBusy === 'activity'"
          maxlength="180"
          placeholder="Optional note"
          type="text"
        />
      </label>
      <div class="flex justify-end">
        <button
          type="submit"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3.5 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="pepBusy !== null"
        >
          <Briefcase class="h-3.5 w-3.5" aria-hidden="true" />
          {{ pepBusy === 'activity' ? "Publishing..." : "Publish activity" }}
        </button>
      </div>
    </form>

    <form v-else-if="composerMode === 'tune'" class="grid gap-3" @submit.prevent="publishTune">
      <div class="grid gap-3 sm:grid-cols-2">
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Title</span>
          <input
            v-model="tuneDraft.title"
            :class="fieldClass"
            :disabled="pepBusy === 'tune'"
            maxlength="160"
            type="text"
          />
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Artist</span>
          <input
            v-model="tuneDraft.artist"
            :class="fieldClass"
            :disabled="pepBusy === 'tune'"
            maxlength="160"
            type="text"
          />
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Source</span>
          <input
            v-model="tuneDraft.source"
            :class="fieldClass"
            :disabled="pepBusy === 'tune'"
            maxlength="160"
            type="text"
          />
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">URI</span>
          <input
            v-model="tuneDraft.uri"
            :class="fieldClass"
            :disabled="pepBusy === 'tune'"
            maxlength="240"
            type="text"
          />
          <span v-if="tuneErrors.uri" class="type-caption text-destructive">{{ tuneErrors.uri }}</span>
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Length</span>
          <input
            v-model="tuneDraft.length"
            :class="fieldClass"
            :disabled="pepBusy === 'tune'"
            inputmode="numeric"
            type="text"
          />
          <span v-if="tuneErrors.length" class="type-caption text-destructive">{{ tuneErrors.length }}</span>
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Rating</span>
          <input
            v-model="tuneDraft.rating"
            :class="fieldClass"
            :disabled="pepBusy === 'tune'"
            inputmode="numeric"
            type="text"
          />
          <span v-if="tuneErrors.rating" class="type-caption text-destructive">{{ tuneErrors.rating }}</span>
        </label>
      </div>
      <p v-if="tuneErrors.form" class="type-caption text-destructive">{{ tuneErrors.form }}</p>
      <div class="flex justify-end">
        <button
          type="submit"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3.5 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="pepBusy !== null"
        >
          <Music class="h-3.5 w-3.5" aria-hidden="true" />
          {{ pepBusy === 'tune' ? "Publishing..." : "Publish tune" }}
        </button>
      </div>
    </form>

    <form v-else class="grid gap-3" @submit.prevent="publishProfile">
      <div class="grid gap-3 sm:grid-cols-2">
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Full name</span>
          <input
            v-model="profileDraft.fullName"
            :class="fieldClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="160"
            type="text"
          />
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Nickname</span>
          <input
            v-model="profileDraft.nickname"
            :class="fieldClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="80"
            type="text"
          />
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Pronouns</span>
          <input
            v-model="profileDraft.pronouns"
            :class="fieldClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="48"
            type="text"
          />
        </label>
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">URL</span>
          <input
            v-model="profileDraft.url"
            :class="fieldClass"
            :disabled="profileLoading || pepBusy === 'profile'"
            maxlength="240"
            type="url"
          />
        </label>
      </div>
      <label class="grid gap-1.5">
        <span :class="fieldLabelClass">Bio</span>
        <textarea
          v-model="profileDraft.note"
          :class="textareaClass"
          :disabled="profileLoading || pepBusy === 'profile'"
          maxlength="500"
        />
      </label>
      <div class="flex items-center justify-between gap-2">
        <button
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="profileLoading || pepBusy !== null"
          @click="loadProfileDraft"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': profileLoading }" aria-hidden="true" />
          {{ profileLoading ? "Loading..." : "Reload" }}
        </button>
        <button
          type="submit"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3.5 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
          :disabled="profileLoading || pepBusy !== null || !profileLoaded"
        >
          <IdCard class="h-3.5 w-3.5" aria-hidden="true" />
          {{ pepBusy === 'profile' ? "Publishing..." : "Publish profile" }}
        </button>
      </div>
    </form>

    <p v-if="composerFeedback" class="type-caption" :class="feedbackClass(composerFeedback.tone)" role="status">
      {{ composerFeedback.message }}
    </p>
    <p v-if="composerMode === 'story' && storyComposerError" class="type-caption text-destructive" role="status">
      {{ storyComposerError }}
    </p>
  </section>
</template>
