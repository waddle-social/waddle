<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { MessageCircle, X } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import { formatPepKeyword } from "@/lib/status-publication-ui";
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

const ringClass = computed(() => {
  if (props.member.status === "speaking") return "huddle-ring huddle-ring--speaking";
  if (inHuddle.value || props.member.inCall) return "huddle-ring";
  return "";
});
</script>

<template>
  <section class="member-profile" :aria-label="`${member.name} profile`">
    <div class="flex items-center justify-between">
      <span class="community-kicker">Profile</span>
      <button
        type="button"
        class="community-page__mobile-nav"
        aria-label="Close profile"
        @click="emit('close')"
      >
        <X class="h-4 w-4" aria-hidden="true" />
      </button>
    </div>

    <div class="member-profile__header">
      <span :class="ringClass">
        <AppAvatar
          :name="member.name"
          :src="member.avatarUrl"
          :presence="member.presence"
          :in-call="member.inCall"
          size="lg"
        />
      </span>
      <div>
        <div class="member-profile__name">{{ member.name }}</div>
        <div class="type-caption text-muted-foreground">{{ member.jid }}</div>
      </div>
      <div class="flex flex-wrap items-center justify-center gap-2">
        <span v-if="member.affiliation" class="community-kicker">{{ member.affiliation }}</span>
        <span
          v-if="member.statusText"
          class="type-caption"
          :class="inHuddle ? 'text-live-text' : 'text-muted-foreground'"
        >{{ member.statusText }}</span>
      </div>
    </div>

    <div v-if="inHuddle" class="member-profile__banner">
      <span class="community-kicker community-kicker--live">
        <span class="community-ember" aria-hidden="true" />
        {{ member.status === "speaking" ? "Speaking now" : "In a huddle" }}
      </span>
    </div>

    <div v-if="loading" class="member-profile" aria-busy="true" aria-label="Loading profile">
      <div class="member-profile__section">
        <Skeleton width="4rem" height="0.55rem" />
        <Skeleton width="65%" height="0.85rem" />
      </div>
      <div class="member-profile__section">
        <Skeleton width="5rem" height="0.55rem" />
        <Skeleton width="50%" height="0.85rem" />
      </div>
    </div>

    <template v-if="!loading">
      <section v-if="fullNameToShow" class="member-profile__section">
        <span class="community-kicker">Name</span>
        <span>{{ fullNameToShow }}</span>
      </section>
      <section v-if="vcard?.pronouns" class="member-profile__section">
        <span class="community-kicker">Pronouns</span>
        <span>{{ vcard.pronouns }}</span>
      </section>
      <section v-if="vcard?.note" class="member-profile__section">
        <span class="community-kicker">Bio</span>
        <span class="whitespace-pre-line">{{ vcard.note }}</span>
      </section>
      <section v-if="safeWebsite" class="member-profile__section">
        <span class="community-kicker">Website</span>
        <a
          :href="safeWebsite"
          target="_blank"
          rel="noreferrer noopener"
          class="break-all underline underline-offset-2 hover:text-primary"
        >{{ safeWebsite }}</a>
      </section>
      <section v-if="pepProfile?.mood" class="member-profile__section">
        <span class="community-kicker">Mood</span>
        <span>
          {{ formatPepKeyword(pepProfile.mood.kind) }}
          <span v-if="pepProfile.mood.text" class="text-muted-foreground"> &mdash; {{ pepProfile.mood.text }}</span>
        </span>
      </section>
      <section v-if="pepProfile?.activity" class="member-profile__section">
        <span class="community-kicker">Activity</span>
        <span>
          {{ formatPepKeyword(pepProfile.activity.general) }}
          <span v-if="pepProfile.activity.specific"> &middot; {{ formatPepKeyword(pepProfile.activity.specific) }}</span>
          <span v-if="pepProfile.activity.text" class="text-muted-foreground"> &mdash; {{ pepProfile.activity.text }}</span>
        </span>
      </section>
      <section v-if="pepProfile?.tune" class="member-profile__section">
        <span class="community-kicker">Listening to</span>
        <span>
          <template v-if="pepProfile.tune.title">{{ pepProfile.tune.title }}</template>
          <template v-if="pepProfile.tune.artist"> &mdash; {{ pepProfile.tune.artist }}</template>
          <span v-if="pepProfile.tune.source" class="text-muted-foreground"> ({{ pepProfile.tune.source }})</span>
        </span>
      </section>
    </template>

    <button
      v-if="!isSelf"
      type="button"
      class="community-pill community-pill--primary justify-center"
      :aria-label="`Message ${member.name}`"
      @click="emit('message', member.jid)"
    >
      <MessageCircle class="h-4 w-4" aria-hidden="true" />
      Message
    </button>
  </section>
</template>
