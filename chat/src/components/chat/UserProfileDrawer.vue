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
  <AppDrawer v-model:open="open" side="right" width-class="w-[min(88vw,20rem)]">
    <template #title>
      <span class="text-[14px] font-semibold">Profile</span>
    </template>

    <div class="p-4 space-y-4">
      <!-- Header -->
      <div class="flex flex-col items-center text-center gap-2 pt-2">
        <AppAvatar :name="username" :src="avatarUrl ?? null" size="lg" :presence="presence" />
        <div>
          <div class="text-[15px] font-semibold">{{ username }}</div>
          <div class="text-[12px] text-muted-foreground">{{ presenceText ?? "offline" }}</div>
        </div>
      </div>

      <!-- Role badges -->
      <div v-if="hats?.length" class="flex flex-wrap justify-center gap-1.5">
        <span
          v-for="hat in hats"
          :key="hat.uri"
          class="inline-flex items-center rounded-full border border-border bg-muted/30 px-2 py-0.5 text-[11px] font-medium text-muted-foreground"
        >
          {{ hat.title }}
        </span>
      </div>

      <!-- Loading skeleton -->
      <div v-if="loading" class="space-y-3">
        <div class="rounded-lg border border-border bg-muted/20 px-3 py-2.5 animate-pulse">
          <div class="h-3 w-16 rounded bg-muted/50 mb-2" />
          <div class="h-4 w-2/3 rounded bg-muted/50" />
        </div>
        <div class="rounded-lg border border-border bg-muted/20 px-3 py-2.5 animate-pulse">
          <div class="h-3 w-20 rounded bg-muted/50 mb-2" />
          <div class="h-4 w-1/2 rounded bg-muted/50" />
        </div>
      </div>

      <!-- PEP sections -->
      <template v-if="!loading && pepProfile">
        <!-- Mood -->
        <section v-if="pepProfile.mood" class="rounded-lg border border-border bg-muted/20 px-3 py-2.5">
          <div class="text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/75 mb-1">Mood</div>
          <div class="text-[13px]">
            {{ formatPepKeyword(pepProfile.mood.kind) }}
            <span v-if="pepProfile.mood.text" class="text-muted-foreground"> &mdash; {{ pepProfile.mood.text }}</span>
          </div>
        </section>

        <!-- Activity -->
        <section v-if="pepProfile.activity" class="rounded-lg border border-border bg-muted/20 px-3 py-2.5">
          <div class="text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/75 mb-1">Activity</div>
          <div class="text-[13px]">
            {{ formatPepKeyword(pepProfile.activity.general) }}
            <span v-if="pepProfile.activity.specific"> &middot; {{ formatPepKeyword(pepProfile.activity.specific) }}</span>
            <span v-if="pepProfile.activity.text" class="text-muted-foreground"> &mdash; {{ pepProfile.activity.text }}</span>
          </div>
        </section>

        <!-- Tune -->
        <section v-if="pepProfile.tune" class="rounded-lg border border-border bg-muted/20 px-3 py-2.5">
          <div class="text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/75 mb-1">Listening To</div>
          <div class="text-[13px]">
            <template v-if="pepProfile.tune.title">{{ pepProfile.tune.title }}</template>
            <template v-if="pepProfile.tune.artist"> &mdash; {{ pepProfile.tune.artist }}</template>
            <span v-if="pepProfile.tune.source" class="text-muted-foreground"> ({{ pepProfile.tune.source }})</span>
          </div>
        </section>
      </template>

      <!-- Message button -->
      <button
        class="flex w-full items-center justify-center gap-2 h-9 rounded-lg bg-primary text-primary-foreground text-[13px] font-semibold hover:shadow-[0_0_16px_var(--glow-strong)] transition-all duration-200"
        @click="emit('message')"
      >
        <MessageCircle class="h-4 w-4" />
        Message
      </button>
    </div>
  </AppDrawer>
</template>
