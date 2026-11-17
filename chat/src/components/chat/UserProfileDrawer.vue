<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { MessageCircle } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, kicker, tag } from "styled-system/recipes";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import { formatPepKeyword } from "@/lib/status-publication-ui";
import { isBotJid } from "@/lib/avatars/author-jid";
import { identityHats } from "./message-card-badges";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { OccupantHat, OccupantPresence, UserPepProfile } from "@/lib/xmpp-client";
import type { VCard4Profile } from "@/lib/xmpp/vcard4-types";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  username: string;
  /** Real bare JID, or "" when unknown (an anonymous room occupant). */
  jid: string;
  /** Real bare JID to show the avatar for; `null` = initials. */
  avatarJid?: string | null;
  presence?: OccupantPresence;
  presenceText?: string;
  hats?: OccupantHat[];
  isSelf?: boolean;
  xmppClient?: BrowserXmppClient | null;
}>();

const emit = defineEmits<{
  message: [];
}>();

const pepProfile = ref<UserPepProfile | null>(null);
const vcard = ref<VCard4Profile | null>(null);
const loading = ref(false);
const shownHats = computed(() => identityHats(props.hats, props.jid));
/** Only a real JID can be messaged; never emit a DM for an unknown one. */
const canMessage = computed(() => !!props.jid && !props.isSelf && !isBotJid(props.jid));

function messagePerson() {
  if (!canMessage.value) return;
  emit("message");
}

watch(
  () => [open.value, props.jid] as const,
  async ([isOpen, jid]) => {
    if (!isOpen || !jid || !props.xmppClient) {
      if (!isOpen) {
        pepProfile.value = null;
        vcard.value = null;
      }
      return;
    }
    loading.value = true;
    // Fetch PEP (mood/activity/tune) and XEP-0292 vCard4 in parallel —
    // they're independent reads and the drawer needs both to render
    // a complete profile. `Promise.allSettled` so a transient failure
    // on one source still surfaces the other (e.g. an older server
    // without vCard4 support still shows mood/activity).
    const [pepResult, vcardResult] = await Promise.allSettled([
      props.xmppClient.fetchUserPepProfile(jid),
      props.xmppClient.fetchVCard4(jid),
    ]);
    pepProfile.value = pepResult.status === "fulfilled" ? pepResult.value : null;
    vcard.value = vcardResult.status === "fulfilled" ? vcardResult.value : null;
    loading.value = false;
  },
);

/** Display name only when the vCard FN differs from the existing
 * `username` header (which is usually the localpart or nickname),
 * to avoid the drawer rendering the same string twice. */
const fullNameToShow = computed(() => {
  const fn = vcard.value?.fullName?.trim();
  if (!fn) return null;
  return fn === props.username ? null : fn;
});

/** Treat the vCard URL as safe only when it parses as an http(s) URL —
 * mirrors the `safeUrl` helper used by ExtensionRouteView to keep the
 * drawer from rendering `javascript:` or `data:` hrefs. */
const safeWebsite = computed(() => {
  const raw = vcard.value?.url?.trim();
  if (!raw) return null;
  try {
    const parsed = new URL(raw);
    return parsed.protocol === "http:" || parsed.protocol === "https:" ? parsed.toString() : null;
  } catch {
    return null;
  }
});

const hasVCardSection = computed(
  () =>
    !!fullNameToShow.value ||
    !!vcard.value?.pronouns ||
    !!vcard.value?.note ||
    !!safeWebsite.value,
);

// ProfileCard board, drawer variant: centred identity, one Message action,
// then the vCard and PEP fields as a hairline-separated list.
const drawerWidth = css({ width: "min(88vw, 360px)" });
const kickerClass = kicker();
const bodyClass = css({ display: "flex", flexDirection: "column", gap: "4", padding: "{spacing.1} {spacing.4} {spacing.4}" });
const headClass = css({ display: "flex", flexDirection: "column", alignItems: "center", gap: "1", textAlign: "center" });
const nameClass = css({ marginTop: "2", font: "700 20px/1.25 {fonts.display}", overflowWrap: "anywhere" });
const jidClass = css({ font: "13px/1.4 {fonts.mono}", color: "fg.muted", wordBreak: "break-all" });
const captionClass = css({ textStyle: "caption", color: "fg.muted" });
const hatsClass = css({ display: "flex", flexWrap: "wrap", justifyContent: "center", gap: "1.5", marginTop: "1.5" });
const tagClass = tag();
const messageClass = cx(btn({ variant: "primary" }), css({ justifyContent: "center", width: "100%" }));
const listClass = css({ margin: 0, display: "flex", flexDirection: "column" });
const fieldClass = css({
  display: "flex",
  flexDirection: "column",
  gap: "1",
  padding: "{spacing.3} 0",
  borderTop: "1px solid {colors.border.soft}",
  "& dd": { margin: 0, fontSize: "14px", lineHeight: 1.5, overflowWrap: "anywhere" },
});
const noteClass = css({ whiteSpace: "pre-line" });
const mutedClass = css({ color: "fg.muted" });
const linkClass = css({ color: "action.text", wordBreak: "break-all", textDecoration: "underline", textUnderlineOffset: "2px" });
const loadingClass = css({ display: "flex", flexDirection: "column", gap: "2.5" });
</script>

<template>
  <AppDrawer v-model:open="open" side="right" :width-class="drawerWidth" label="Profile drawer">
    <template #title>
      <span :class="kickerClass">Profile</span>
    </template>

    <div :class="bodyClass">
      <div :class="headClass">
        <UserAvatar :name="username" :jid="avatarJid ?? null" size="xl" :presence="presence" />
        <span :class="nameClass">{{ username }}</span>
        <span v-if="jid && jid !== username" :class="jidClass">{{ jid }}</span>
        <span v-if="!isBotJid(jid)" :class="captionClass">{{ presenceText ?? "offline" }}</span>
        <span v-if="shownHats.length" :class="hatsClass">
          <span v-for="hat in shownHats" :key="hat.uri" :class="tagClass">{{ hat.title }}</span>
        </span>
      </div>

      <!-- Message (hidden for own profile and bots, which take no DMs) -->
      <button
        v-if="canMessage"
        :class="messageClass"
        type="button"
        aria-label="Message this person"
        @click="messagePerson"
      >
        <MessageCircle aria-hidden="true" />
        Message
      </button>

      <!-- Two skeleton blocks cover the parallel vCard + PEP fetches. -->
      <div v-if="loading" :class="loadingClass" aria-busy="true" aria-label="Loading profile">
        <Skeleton height="64px" radius="var(--radii-sm)" />
        <Skeleton height="40px" radius="var(--radii-sm)" />
      </div>

      <!-- vCard4 identity first (durable), then volatile PEP mood/activity/tune. -->
      <dl v-if="!loading && (hasVCardSection || pepProfile)" :class="listClass">
        <template v-if="hasVCardSection">
          <div v-if="fullNameToShow" :class="fieldClass">
            <dt :class="kickerClass">Name</dt>
            <dd>{{ fullNameToShow }}</dd>
          </div>
          <div v-if="vcard?.pronouns" :class="fieldClass">
            <dt :class="kickerClass">Pronouns</dt>
            <dd>{{ vcard.pronouns }}</dd>
          </div>
          <div v-if="vcard?.note" :class="fieldClass">
            <dt :class="kickerClass">Bio</dt>
            <dd :class="noteClass">{{ vcard.note }}</dd>
          </div>
          <div v-if="safeWebsite" :class="fieldClass">
            <dt :class="kickerClass">Website</dt>
            <dd>
              <a :href="safeWebsite" target="_blank" rel="noreferrer noopener" :class="linkClass">{{ safeWebsite }}</a>
            </dd>
          </div>
        </template>

        <template v-if="pepProfile">
          <div v-if="pepProfile.mood" :class="fieldClass">
            <dt :class="kickerClass">Mood</dt>
            <dd>
              {{ formatPepKeyword(pepProfile.mood.kind) }}
              <span v-if="pepProfile.mood.text" :class="mutedClass"> &mdash; {{ pepProfile.mood.text }}</span>
            </dd>
          </div>
          <div v-if="pepProfile.activity" :class="fieldClass">
            <dt :class="kickerClass">Activity</dt>
            <dd>
              {{ formatPepKeyword(pepProfile.activity.general) }}
              <span v-if="pepProfile.activity.specific"> &middot; {{ formatPepKeyword(pepProfile.activity.specific) }}</span>
              <span v-if="pepProfile.activity.text" :class="mutedClass"> &mdash; {{ pepProfile.activity.text }}</span>
            </dd>
          </div>
          <div v-if="pepProfile.tune" :class="fieldClass">
            <dt :class="kickerClass">Listening to</dt>
            <dd>
              <template v-if="pepProfile.tune.title">{{ pepProfile.tune.title }}</template>
              <template v-if="pepProfile.tune.artist"> &mdash; {{ pepProfile.tune.artist }}</template>
              <span v-if="pepProfile.tune.source" :class="mutedClass"> ({{ pepProfile.tune.source }})</span>
            </dd>
          </div>
        </template>
      </dl>
    </div>
  </AppDrawer>
</template>
