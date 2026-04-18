<script setup lang="ts">
import { ChevronLeft, Palette, ScanText, UserRound } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ThemeSwitcher from "@/components/chat/ThemeSwitcher.vue";
import ScrollDirectionSwitcher from "@/components/chat/ScrollDirectionSwitcher.vue";
import VersionFooter from "@/components/chat/VersionFooter.vue";
import type { ServerVersion } from "@/composables/useVersion";
import type { WaddleSession } from "@/lib/server-auth";

defineProps<{
  session: WaddleSession;
  webCommitSha?: string;
  serverVersion?: ServerVersion | null;
}>();

const emit = defineEmits<{
  close: [];
}>();
</script>

<template>
  <div class="flex-1 min-w-0 min-h-0 overflow-auto bg-background">
    <div class="sticky top-0 z-10 border-b border-border glass-surface">
      <div class="mx-auto flex max-w-3xl items-center gap-3 px-4 py-3 sm:px-6">
        <button
          class="flex h-9 items-center gap-1.5 rounded-xl border border-border/70 px-3 text-[12px] font-medium text-muted-foreground transition-colors duration-200 hover:bg-muted hover:text-foreground"
          @click="emit('close')"
        >
          <ChevronLeft class="h-4 w-4" />
          <span>Back</span>
        </button>
        <div class="min-w-0">
          <h1 class="font-display text-[16px] font-bold tracking-tight">Settings</h1>
          <p class="text-[12px] text-muted-foreground">Tune how Waddle looks and how timelines read.</p>
        </div>
      </div>
    </div>

    <div class="mx-auto flex max-w-3xl flex-col gap-4 px-4 py-6 sm:px-6">
      <section class="glass-panel rounded-2xl border border-border p-4 sm:p-5">
        <div class="flex items-center gap-3">
          <AppAvatar :name="session.username" :src="session.avatar_url" size="md" />
          <div class="min-w-0">
            <div class="flex items-center gap-2 text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/70">
              <UserRound class="h-3.5 w-3.5" />
              <span>Signed in</span>
            </div>
            <h2 class="mt-1 truncate text-[16px] font-semibold">{{ session.username }}</h2>
            <p class="text-[12px] text-muted-foreground">Personal preferences follow your device theme by default.</p>
          </div>
        </div>
      </section>

      <section class="glass-panel rounded-2xl border border-border p-4 sm:p-5">
        <div class="mb-4 flex items-start gap-3">
          <div class="flex h-9 w-9 items-center justify-center rounded-xl bg-primary/10 text-primary">
            <Palette class="h-4 w-4" />
          </div>
          <div>
            <h2 class="text-[14px] font-semibold">Appearance</h2>
            <p class="mt-1 text-[12px] leading-relaxed text-muted-foreground">
              Light, dark, or follow the system automatically.
            </p>
          </div>
        </div>
        <ThemeSwitcher />
      </section>

      <section class="glass-panel rounded-2xl border border-border p-4 sm:p-5">
        <div class="mb-4 flex items-start gap-3">
          <div class="flex h-9 w-9 items-center justify-center rounded-xl bg-primary/10 text-primary">
            <ScanText class="h-4 w-4" />
          </div>
          <div>
            <h2 class="text-[14px] font-semibold">Reading flow</h2>
            <p class="mt-1 text-[12px] leading-relaxed text-muted-foreground">
              Set whether the newest messages land at the bottom or the top.
            </p>
          </div>
        </div>
        <ScrollDirectionSwitcher />
      </section>

      <section class="glass-panel rounded-2xl border border-border p-4 sm:p-5">
        <h2 class="text-[14px] font-semibold">About this build</h2>
        <p class="mt-1 text-[12px] leading-relaxed text-muted-foreground">
          Helpful version details when you need to compare client and server behavior.
        </p>
        <div class="mt-4">
          <VersionFooter
            :web-commit-sha="webCommitSha"
            :server-version="serverVersion"
            layout="detail"
          />
        </div>
      </section>
    </div>
  </div>
</template>
