<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { MessageCircle } from "lucide-vue-next";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import { formatPepKeyword } from "@/lib/status-publication-ui";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { OccupantHat, OccupantPresence, UserPepProfile } from "@/lib/xmpp-client";
import type { VCard4Profile } from "@/lib/xmpp/vcard4-types";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  username: string;
  jid: string;
  avatarUrl?: string | null;
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
</script>

<template>
  <AppDrawer v-model:open="open" side="right" width-class="w-[var(--chat-profile-drawer-width)]" label="Profile drawer">
    <template #title>
      <span class="type-pane-title">Profile</span>
    </template>

    <div class="chat-dialog-stack p-4">
      <!-- Header -->
      <div class="flex flex-col items-center gap-2 pt-2 text-center">
        <AppAvatar :name="username" :src="avatarUrl ?? null" size="lg" :presence="presence" />
        <div>
          <div class="type-card-title">{{ username }}</div>
          <div class="type-caption text-muted-foreground">{{ presenceText ?? "offline" }}</div>
        </div>
      </div>

      <!-- Role badges -->
      <div v-if="hats?.length" class="flex flex-wrap justify-center gap-1.5">
        <span
          v-for="hat in hats"
          :key="hat.uri"
          class="type-caption type-emphasis inline-flex h-7 items-center rounded-full border border-border bg-muted/30 px-2 text-muted-foreground"
        >
          {{ hat.title }}
        </span>
      </div>

      <!-- Loading skeleton — uses the shared Skeleton component so
           the luminous-shimmer treatment (iter-1 chat-skeleton
           animation) is consistent with every other loading
           placeholder in the app. Two skeleton blocks cover the
           parallel vCard + PEP fetches. -->
      <div v-if="loading" class="chat-panel-stack" aria-busy="true" aria-label="Loading profile">
        <div class="flex flex-col gap-2 rounded-lg border border-border bg-muted/20 px-3 py-3">
          <Skeleton width="4rem" height="0.55rem" />
          <Skeleton width="65%" height="0.85rem" />
        </div>
        <div class="flex flex-col gap-2 rounded-lg border border-border bg-muted/20 px-3 py-3">
          <Skeleton width="5rem" height="0.55rem" />
          <Skeleton width="50%" height="0.85rem" />
        </div>
      </div>

      <!-- vCard4 identity sections (Name / Pronouns / Bio / Website).
           Rendered above mood/activity/tune because vCard is durable
           identity while PEP mood/activity/tune is volatile presence. -->
      <template v-if="!loading && hasVCardSection">
        <section v-if="fullNameToShow" class="rounded-lg border border-border bg-muted/20 px-3 py-3">
          <div class="type-section-label text-muted-foreground/75">Name</div>
          <div class="type-field">{{ fullNameToShow }}</div>
        </section>

        <section v-if="vcard?.pronouns" class="rounded-lg border border-border bg-muted/20 px-3 py-3">
          <div class="type-section-label text-muted-foreground/75">Pronouns</div>
          <div class="type-field">{{ vcard.pronouns }}</div>
        </section>

        <section v-if="vcard?.note" class="rounded-lg border border-border bg-muted/20 px-3 py-3">
          <div class="type-section-label text-muted-foreground/75">Bio</div>
          <div class="type-field whitespace-pre-line">{{ vcard.note }}</div>
        </section>

        <section v-if="safeWebsite" class="rounded-lg border border-border bg-muted/20 px-3 py-3">
          <div class="type-section-label text-muted-foreground/75">Website</div>
          <div class="type-field">
            <a
              :href="safeWebsite"
              target="_blank"
              rel="noreferrer noopener"
              class="break-all underline underline-offset-2 hover:text-primary"
            >
              {{ safeWebsite }}
            </a>
          </div>
        </section>
      </template>

      <!-- PEP sections (presence: mood / activity / tune) -->
      <template v-if="!loading && pepProfile">
        <!-- Mood -->
        <section v-if="pepProfile.mood" class="rounded-lg border border-border bg-muted/20 px-3 py-3">
          <div class="type-section-label text-muted-foreground/75">Mood</div>
          <div class="type-field">
            {{ formatPepKeyword(pepProfile.mood.kind) }}
            <span v-if="pepProfile.mood.text" class="text-muted-foreground"> &mdash; {{ pepProfile.mood.text }}</span>
          </div>
        </section>

        <!-- Activity -->
        <section v-if="pepProfile.activity" class="rounded-lg border border-border bg-muted/20 px-3 py-3">
          <div class="type-section-label text-muted-foreground/75">Activity</div>
          <div class="type-field">
            {{ formatPepKeyword(pepProfile.activity.general) }}
            <span v-if="pepProfile.activity.specific"> &middot; {{ formatPepKeyword(pepProfile.activity.specific) }}</span>
            <span v-if="pepProfile.activity.text" class="text-muted-foreground"> &mdash; {{ pepProfile.activity.text }}</span>
          </div>
        </section>

        <!-- Tune -->
        <section v-if="pepProfile.tune" class="rounded-lg border border-border bg-muted/20 px-3 py-3">
          <div class="type-section-label text-muted-foreground/75">Listening to</div>
          <div class="type-field">
            <template v-if="pepProfile.tune.title">{{ pepProfile.tune.title }}</template>
            <template v-if="pepProfile.tune.artist"> &mdash; {{ pepProfile.tune.artist }}</template>
            <span v-if="pepProfile.tune.source" class="text-muted-foreground"> ({{ pepProfile.tune.source }})</span>
          </div>
        </section>
      </template>

      <!-- Message button (hidden for own profile) -->
      <button
        v-if="!isSelf"
        class="chat-action-button chat-action-button--primary type-action w-full"
        type="button"
        aria-label="Message this person"
        @click="emit('message')"
      >
        <MessageCircle class="h-4 w-4" />
        Message
      </button>
    </div>
  </AppDrawer>
</template>
