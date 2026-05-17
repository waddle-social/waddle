<script setup lang="ts">
import { onMounted, reactive, ref, watch } from "vue";
import { IdCard } from "lucide-vue-next";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import {
  draftFromProfile,
  profileFromDraft,
  profilesEqual,
  type VCard4Draft,
  type VCard4Profile,
} from "@/lib/xmpp/vcard4-types";

const props = defineProps<{
  xmppClient?: BrowserXmppClient | null;
  selfJid: string;
}>();

type FeedbackTone = "muted" | "success" | "error";

interface VCardFeedback {
  tone: FeedbackTone;
  message: string;
}

const fieldLabelClass = "type-section-label text-muted-foreground/75";
const fieldClass = "chat-field-control type-field disabled:opacity-50";
const textareaClass = "chat-field-control chat-textarea-control type-field disabled:opacity-50";
const primaryButtonClass = "chat-action-button chat-action-button--primary type-control disabled:opacity-40";
const secondaryButtonClass = "chat-action-button chat-action-button--secondary type-control disabled:opacity-40";

const draft = reactive<VCard4Draft>(draftFromProfile(null));
const loading = ref(false);
const saving = ref(false);
/** Last value seen from the server — the rollback target on save failure. */
const lastPersisted = ref<VCard4Profile>({});
const feedback = ref<VCardFeedback>({
  tone: "muted",
  message: "Add your name, nickname, pronouns, bio, and a link — publish to share over XMPP.",
});

function applyDraft(profile: VCard4Profile) {
  const next = draftFromProfile(profile);
  draft.fullName = next.fullName;
  draft.nickname = next.nickname;
  draft.pronouns = next.pronouns;
  draft.note = next.note;
  draft.url = next.url;
}

async function loadProfile() {
  if (!props.xmppClient) {
    feedback.value = {
      tone: "muted",
      message: "Reconnect to load your profile.",
    };
    return;
  }
  loading.value = true;
  try {
    const profile = await props.xmppClient.fetchVCard4(props.selfJid);
    const next = profile ?? {};
    lastPersisted.value = next;
    applyDraft(next);
    feedback.value = profile
      ? {
          tone: "muted",
          message: "Loaded from your vCard4 PEP node. Edit and publish to share an update.",
        }
      : {
          tone: "muted",
          message: "No profile saved yet — fill in what you want to share.",
        };
  } catch (error) {
    feedback.value = {
      tone: "error",
      message: error instanceof Error
        ? `Couldn't load profile: ${error.message}`
        : "Couldn't load your profile.",
    };
  } finally {
    loading.value = false;
  }
}

onMounted(() => {
  void loadProfile();
});

watch(
  () => props.xmppClient,
  (client, previous) => {
    if (client && client !== previous) {
      void loadProfile();
    }
  },
);

async function saveProfile() {
  if (!props.xmppClient) {
    feedback.value = {
      tone: "error",
      message: "Reconnect before publishing your profile.",
    };
    return;
  }
  const next = profileFromDraft(draft);
  if (profilesEqual(next, lastPersisted.value)) {
    feedback.value = {
      tone: "muted",
      message: "Nothing to publish — your draft matches the saved profile.",
    };
    return;
  }
  const previous = lastPersisted.value;
  // Optimistic update — show the new value as persisted before the server
  // confirms, then roll back to `previous` if the publish fails.
  lastPersisted.value = next;
  saving.value = true;
  try {
    await props.xmppClient.publishVCard4(next);
    feedback.value = {
      tone: "success",
      message: "Profile published.",
    };
  } catch (error) {
    lastPersisted.value = previous;
    applyDraft(previous);
    feedback.value = {
      tone: "error",
      message: error instanceof Error
        ? `Couldn't publish: ${error.message}`
        : "Couldn't publish your profile. Rolled back to the last saved version.",
    };
  } finally {
    saving.value = false;
  }
}

function revertDraft() {
  applyDraft(lastPersisted.value);
  feedback.value = {
    tone: "muted",
    message: "Reverted to the last saved profile.",
  };
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
</script>

<template>
  <section class="chat-section-card glass-panel">
    <div class="flex items-start gap-3 pb-4">
      <div class="flex h-8 w-8 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <IdCard class="h-4 w-4" />
      </div>
      <div class="chat-settings-copy max-w-2xl">
        <p class="type-section-label text-muted-foreground/70">XEP-0292</p>
        <h2 class="type-pane-title">Profile (vCard4)</h2>
        <p class="type-caption text-muted-foreground">
          Public profile published over your <code>urn:xmpp:vcard4</code> PEP node.
          Visible to anyone who can read your profile.
        </p>
      </div>
    </div>

    <p
      v-if="!xmppClient"
      class="type-caption mb-4 rounded-lg border border-border/70 bg-muted/50 px-3 py-2 text-muted-foreground"
      role="status"
    >
      Reconnect before loading or publishing your profile.
    </p>

    <form class="chat-settings-form" @submit.prevent="saveProfile">
      <div class="grid gap-3 sm:grid-cols-2">
        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Full name</span>
          <input
            v-model="draft.fullName"
            :class="fieldClass"
            :disabled="loading || saving"
            data-testid="vcard4-fn"
            maxlength="160"
            placeholder="Romeo Montague"
            type="text"
          />
        </label>

        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Nickname</span>
          <input
            v-model="draft.nickname"
            :class="fieldClass"
            :disabled="loading || saving"
            data-testid="vcard4-nickname"
            maxlength="80"
            placeholder="Romeo"
            type="text"
          />
        </label>

        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">Pronouns</span>
          <input
            v-model="draft.pronouns"
            :class="fieldClass"
            :disabled="loading || saving"
            data-testid="vcard4-pronouns"
            maxlength="48"
            placeholder="he/him, she/her, they/them"
            type="text"
          />
        </label>

        <label class="grid gap-1.5">
          <span :class="fieldLabelClass">URL</span>
          <input
            v-model="draft.url"
            :class="fieldClass"
            :disabled="loading || saving"
            data-testid="vcard4-url"
            maxlength="240"
            placeholder="https://romeo.example.com"
            type="url"
          />
        </label>

        <label class="grid gap-1.5 sm:col-span-2">
          <span :class="fieldLabelClass">Bio</span>
          <textarea
            v-model="draft.note"
            :class="textareaClass"
            :disabled="loading || saving"
            data-testid="vcard4-note"
            maxlength="500"
            placeholder="A short note about you. Plain text only."
            rows="3"
          />
        </label>
      </div>

      <div class="flex flex-wrap items-center gap-2 pt-3">
        <button
          type="submit"
          :class="primaryButtonClass"
          :disabled="loading || saving || !xmppClient"
          data-testid="vcard4-save"
        >
          {{ saving ? "Publishing…" : "Save profile" }}
        </button>
        <button
          type="button"
          :class="secondaryButtonClass"
          :disabled="loading || saving || !xmppClient"
          data-testid="vcard4-revert"
          @click="revertDraft"
        >
          Revert
        </button>
        <p
          class="type-caption"
          :class="feedbackClass(feedback.tone)"
          aria-live="polite"
          data-testid="vcard4-feedback"
        >
          {{ feedback.message }}
        </p>
      </div>
    </form>
  </section>
</template>
