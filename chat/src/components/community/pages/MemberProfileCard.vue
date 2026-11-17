<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { MessageCircle, X } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, iconbtn } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { pageClasses as page } from "@/components/community/pages/page-classes";
import Skeleton from "@/components/ui/Skeleton.vue";
import { formatPepKeyword } from "@/lib/status-publication-ui";
import { isBotJid } from "@/lib/avatars/author-jid";
import type { BrowserXmppClient, UserPepProfile } from "@/lib/xmpp-client";
import type { VCard4Profile } from "@/lib/xmpp/vcard4-types";
import type { MemberCardModel } from "@/shell/controllers/use-people-rail";

/**
 * Inline profile for the Members page context column. Same data as
 * `UserProfileDrawer` (vCard4 + PEP mood/activity/tune fetched on demand)
 * rendered as a column card instead of a drawer.
 */
const props = defineProps<{
  member: MemberCardModel;
  xmppClient?: BrowserXmppClient | null;
  isSelf?: boolean;
}>();

const emit = defineEmits<{
  message: [jid: string];
  close: [];
}>();

const pepProfile = ref<UserPepProfile | null>(null);
const vcard = ref<VCard4Profile | null>(null);
const loading = ref(false);

watch(
  () => [props.member.jid, props.xmppClient] as const,
  async ([jid, client]) => {
    pepProfile.value = null;
    vcard.value = null;
    if (!jid || !client) return;
    loading.value = true;
    const [pepResult, vcardResult] = await Promise.allSettled([
      client.fetchUserPepProfile(jid),
      client.fetchVCard4(jid),
    ]);
    // A newer selection may have superseded this fetch.
    if (jid !== props.member.jid) return;
    pepProfile.value = pepResult.status === "fulfilled" ? pepResult.value : null;
    vcard.value = vcardResult.status === "fulfilled" ? vcardResult.value : null;
    loading.value = false;
  },
  { immediate: true },
);

const fullNameToShow = computed(() => {
  const fn = vcard.value?.fullName?.trim();
  if (!fn || fn === props.member.name) return null;
  return fn;
});

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

const inHuddle = computed(() => props.member.status === "in-huddle" || props.member.status === "speaking");

// ProfileCard board: centred identity, one Message action, then a
// definition list with hairline separators.
const root = css({ display: "flex", flexDirection: "column", gap: "4", padding: "4", color: "fg", minWidth: 0 });
const topRow = css({ display: "flex", alignItems: "center", "& > :first-child": { flex: 1 } });
const identity = css({ display: "flex", flexDirection: "column", alignItems: "center", gap: "1", textAlign: "center", minWidth: 0 });
const nameClass = css({ mt: "2", font: "700 20px/1.25 {fonts.display}", overflowWrap: "anywhere" });
const jidClass = css({ font: "400 13px/1.4 {fonts.mono}", color: "fg.muted", overflowWrap: "anywhere" });
const metaClass = css({ font: "400 13px/1.4 {fonts.body}", color: "fg.muted", _firstLetter: { textTransform: "uppercase" } });
const liveText = css({ color: "live.text" });
const messageBtn = cx(btn({ variant: "primary" }), css({ justifyContent: "center", width: "100%" }));
const list = css({ display: "flex", flexDirection: "column", margin: 0 });
const field = css({ display: "flex", flexDirection: "column", gap: "1", py: "3", borderTop: "1px solid {colors.border.soft}" });
const dd = css({ margin: 0, font: "400 14px/1.5 {fonts.body}", color: "fg", overflowWrap: "anywhere" });
const muted = css({ color: "fg.muted" });
const preLine = css({ whiteSpace: "pre-line" });
const link = css({ color: "action.text", textDecoration: "underline", textUnderlineOffset: "2px", wordBreak: "break-all" });
const skeleton = css({ display: "flex", flexDirection: "column", gap: "3" });
const skeletonField = css({ display: "flex", flexDirection: "column", gap: "1.5", py: "3", borderTop: "1px solid {colors.border.soft}" });
const closeBtn = iconbtn();
</script>

<template>
  <section :class="root" :aria-label="`${member.name} profile`">
    <div :class="topRow">
      <span :class="page.kicker">Profile</span>
      <button type="button" :class="closeBtn" aria-label="Close profile" @click="emit('close')">
        <X aria-hidden="true" />
      </button>
    </div>

    <div :class="identity">
      <UserAvatar
        :name="member.name"
        :jid="member.jid"
        :presence="member.presence"
        :in-call="member.inCall"
        :huddle="inHuddle || member.inCall"
        :speaking="member.status === 'speaking'"
        size="xl"
      />
      <div :class="nameClass">{{ member.name }}</div>
      <div :class="jidClass">{{ member.jid }}</div>
      <div v-if="member.affiliation || member.statusText" :class="[metaClass, inHuddle && liveText]">
        <template v-if="member.affiliation">{{ member.affiliation }}</template>
        <template v-if="member.affiliation && member.statusText"> · </template>
        <template v-if="member.statusText">{{ member.statusText }}</template>
      </div>
      <span v-if="inHuddle" :class="page.liveKicker">
        <span :class="page.ember" aria-hidden="true" />
        {{ member.status === "speaking" ? "Speaking now" : "In a huddle" }}
      </span>
    </div>

    <button
      v-if="!isSelf && !isBotJid(member.jid)"
      type="button"
      :class="messageBtn"
      :aria-label="`Message ${member.name}`"
      @click="emit('message', member.jid)"
    >
      <MessageCircle aria-hidden="true" />
      Message
    </button>

    <div v-if="loading" :class="skeleton" aria-busy="true" aria-label="Loading profile">
      <div :class="skeletonField">
        <Skeleton width="4rem" height="0.55rem" />
        <Skeleton width="65%" height="0.85rem" />
      </div>
      <div :class="skeletonField">
        <Skeleton width="5rem" height="0.55rem" />
        <Skeleton width="50%" height="0.85rem" />
      </div>
    </div>

    <dl v-else :class="list">
      <div v-if="fullNameToShow" :class="field">
        <dt :class="page.kicker">Name</dt>
        <dd :class="dd">{{ fullNameToShow }}</dd>
      </div>
      <div v-if="vcard?.pronouns" :class="field">
        <dt :class="page.kicker">Pronouns</dt>
        <dd :class="dd">{{ vcard.pronouns }}</dd>
      </div>
      <div v-if="vcard?.note" :class="field">
        <dt :class="page.kicker">Bio</dt>
        <dd :class="[dd, preLine]">{{ vcard.note }}</dd>
      </div>
      <div v-if="safeWebsite" :class="field">
        <dt :class="page.kicker">Website</dt>
        <dd :class="dd">
          <a :href="safeWebsite" target="_blank" rel="noreferrer noopener" :class="link">{{ safeWebsite }}</a>
        </dd>
      </div>
      <div v-if="pepProfile?.mood" :class="field">
        <dt :class="page.kicker">Mood</dt>
        <dd :class="dd">
          {{ formatPepKeyword(pepProfile.mood.kind) }}
          <span v-if="pepProfile.mood.text" :class="muted"> &mdash; {{ pepProfile.mood.text }}</span>
        </dd>
      </div>
      <div v-if="pepProfile?.activity" :class="field">
        <dt :class="page.kicker">Activity</dt>
        <dd :class="dd">
          {{ formatPepKeyword(pepProfile.activity.general) }}
          <span v-if="pepProfile.activity.specific"> &middot; {{ formatPepKeyword(pepProfile.activity.specific) }}</span>
          <span v-if="pepProfile.activity.text" :class="muted"> &mdash; {{ pepProfile.activity.text }}</span>
        </dd>
      </div>
      <div v-if="pepProfile?.tune" :class="field">
        <dt :class="page.kicker">Listening to</dt>
        <dd :class="dd">
          <template v-if="pepProfile.tune.title">{{ pepProfile.tune.title }}</template>
          <template v-if="pepProfile.tune.artist"> &mdash; {{ pepProfile.tune.artist }}</template>
          <span v-if="pepProfile.tune.source" :class="muted"> ({{ pepProfile.tune.source }})</span>
        </dd>
      </div>
    </dl>
  </section>
</template>
