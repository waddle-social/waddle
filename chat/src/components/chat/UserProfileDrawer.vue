<script setup lang="ts">
import { ref, watch } from "vue";
import { MessageCircle } from "lucide-vue-next";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { formatPepKeyword } from "@/lib/status-publication-ui";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { OccupantHat, OccupantPresence, UserPepProfile } from "@/lib/xmpp-client";

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
const loading = ref(false);

watch(
  () => [open.value, props.jid] as const,
  async ([isOpen, jid]) => {
    if (!isOpen || !jid || !props.xmppClient) {
      if (!isOpen) pepProfile.value = null;
      return;
    }
    loading.value = true;
    try {
      pepProfile.value = await props.xmppClient.fetchUserPepProfile(jid);
    } catch {
      pepProfile.value = null;
    } finally {
      loading.value = false;
    }
  },
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

      <!-- Loading skeleton -->
      <div v-if="loading" class="chat-panel-stack">
        <div class="flex flex-col gap-2 rounded-lg border border-border bg-muted/20 px-3 py-3 animate-pulse">
          <div class="h-3 w-16 rounded bg-muted/50" />
          <div class="h-4 w-2/3 rounded bg-muted/50" />
        </div>
        <div class="flex flex-col gap-2 rounded-lg border border-border bg-muted/20 px-3 py-3 animate-pulse">
          <div class="h-3 w-20 rounded bg-muted/50" />
          <div class="h-4 w-1/2 rounded bg-muted/50" />
        </div>
      </div>

      <!-- PEP sections -->
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
