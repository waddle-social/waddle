<script setup lang="ts">
import { onMounted, reactive, ref, watch } from "vue";
import { css } from "styled-system/css";
import { feedbackClass, settingsClasses as sc, type FeedbackTone } from "@/components/chat/settings-classes";
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

interface VCardFeedback {
  tone: FeedbackTone;
  message: string;
}

const headClass = css({ display: "flex", alignItems: "center", gap: "14px" });
const growClass = css({ flex: 1, minWidth: 0 });
const formClass = css({ display: "flex", flexDirection: "column", gap: "4" });

const draft = reactive<VCard4Draft>(draftFromProfile(null));
const loading = ref(false);
const saving = ref(false);
const profileLoaded = ref(false);
/** Last value seen from the server — the rollback target on save failure. */
const lastPersisted = ref<VCard4Profile>({});
let profileLoadRequestId = 0;
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
  draft.photoUri = next.photoUri;
}

async function loadProfile() {
  const requestId = ++profileLoadRequestId;
  const client = props.xmppClient;
  const selfJid = props.selfJid;
  if (!client) {
    profileLoaded.value = false;
    feedback.value = {
      tone: "muted",
      message: "Reconnect to load your profile.",
    };
    return;
  }
  loading.value = true;
  try {
    const profile = await client.fetchVCard4(selfJid);
    if (
      requestId !== profileLoadRequestId ||
      client !== props.xmppClient ||
      selfJid !== props.selfJid
    ) {
      return;
    }
    const next = profile ?? {};
    lastPersisted.value = next;
    applyDraft(next);
    profileLoaded.value = true;
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
    if (
      requestId !== profileLoadRequestId ||
      client !== props.xmppClient ||
      selfJid !== props.selfJid
    ) {
      return;
    }
    profileLoaded.value = false;
    feedback.value = {
      tone: "error",
      message: error instanceof Error
        ? `Couldn't load profile: ${error.message}`
        : "Couldn't load your profile.",
    };
  } finally {
    if (
      requestId === profileLoadRequestId &&
      client === props.xmppClient &&
      selfJid === props.selfJid
    ) {
      loading.value = false;
    }
  }
}

onMounted(() => {
  void loadProfile();
});

watch(
  () => [props.xmppClient, props.selfJid] as const,
  ([client, selfJid], [previousClient, previousSelfJid]) => {
    if (client === previousClient && selfJid === previousSelfJid) {
      return;
    }
    profileLoaded.value = false;
    lastPersisted.value = {};
    applyDraft({});
    if (!client) {
      profileLoadRequestId += 1;
      loading.value = false;
      feedback.value = {
        tone: "muted",
        message: "Reconnect to load your profile.",
      };
      return;
    }
    void loadProfile();
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
  if (!profileLoaded.value) {
    feedback.value = {
      tone: "error",
      message: "Load your saved profile before publishing changes.",
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
</script>

<template>
  <section :class="sc.card" aria-labelledby="vcard4-title">
    <div :class="headClass">
      <slot name="avatar" />
      <div :class="[sc.cardHead, growClass]">
        <h2 id="vcard4-title" :class="sc.heading">Profile</h2>
        <p :class="sc.caption">Anyone who can see your profile sees this. Published to your vCard4 profile (XEP-0292).</p>
      </div>
    </div>

    <form :class="formClass" @submit.prevent="saveProfile">
      <div :class="sc.grid">
        <label :class="sc.label">
          Full name
          <input
            v-model="draft.fullName"
            :class="sc.input"
            :disabled="loading || saving"
            data-testid="vcard4-fn"
            maxlength="160"
            placeholder="Romeo Montague"
            type="text"
          />
        </label>

        <label :class="sc.label">
          Nickname
          <input
            v-model="draft.nickname"
            :class="sc.input"
            :disabled="loading || saving"
            data-testid="vcard4-nickname"
            maxlength="80"
            placeholder="Romeo"
            type="text"
          />
        </label>

        <label :class="sc.label">
          Pronouns
          <input
            v-model="draft.pronouns"
            :class="sc.input"
            :disabled="loading || saving"
            data-testid="vcard4-pronouns"
            maxlength="48"
            placeholder="he/him, she/her, they/them"
            type="text"
          />
        </label>

        <label :class="sc.label">
          Website
          <input
            v-model="draft.url"
            :class="sc.input"
            :disabled="loading || saving"
            data-testid="vcard4-url"
            maxlength="240"
            placeholder="https://"
            type="url"
          />
        </label>

        <label :class="[sc.label, sc.wide]">
          Bio
          <textarea
            v-model="draft.note"
            :class="sc.textarea"
            :disabled="loading || saving"
            data-testid="vcard4-note"
            maxlength="500"
            placeholder="A short note about you. Plain text only."
            rows="3"
          />
        </label>
      </div>

      <div :class="sc.actions">
        <button
          type="submit"
          :class="sc.primary"
          :disabled="loading || saving || !xmppClient || !profileLoaded"
          data-testid="vcard4-save"
        >
          {{ saving ? "Publishing…" : "Save profile" }}
        </button>
        <button
          type="button"
          :class="sc.secondary"
          :disabled="loading || saving || !xmppClient"
          data-testid="vcard4-revert"
          @click="revertDraft"
        >
          Revert
        </button>
        <p
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
