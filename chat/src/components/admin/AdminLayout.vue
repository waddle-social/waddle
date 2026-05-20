<script setup lang="ts">
// Admin V2 — responsive shell. The left navigation is a pinned 16rem
// column on `lg+` and slides in as a drawer from the left on `<lg`.
// The hamburger toggle in the mobile header opens the drawer; tapping
// the backdrop or selecting a panel closes it.
import { computed, ref } from "vue";
import {
  BellRing,
  ChevronLeft,
  FolderTree,
  Hash,
  Menu,
  ScrollText,
  Settings as SettingsIcon,
  Users,
  X,
} from "lucide-vue-next";
import { routes, type AdminPanel } from "@/router";

interface PanelDef {
  slug: AdminPanel;
  label: string;
  icon: typeof Users;
  enabled: boolean;
}

const props = defineProps<{
  /** Slug of the currently rendered panel. */
  activePanel: AdminPanel;
}>();

const emit = defineEmits<{
  navigate: [path: string];
  back: [];
}>();

const PANELS: ReadonlyArray<PanelDef> = [
  { slug: "users", label: "Users", icon: Users, enabled: true },
  { slug: "spaces", label: "Spaces", icon: FolderTree, enabled: true },
  { slug: "channels", label: "Channels", icon: Hash, enabled: true },
  { slug: "audit", label: "Audit log", icon: ScrollText, enabled: false },
  { slug: "push-health", label: "Push health", icon: BellRing, enabled: false },
  { slug: "settings", label: "Settings", icon: SettingsIcon, enabled: false },
];

const sidebarOpen = ref(false);

function isActive(panel: PanelDef): boolean {
  return panel.slug === props.activePanel;
}

function onPanelClick(panel: PanelDef): void {
  if (!panel.enabled) return;
  sidebarOpen.value = false;
  emit("navigate", routes.admin.href({ params: { panel: panel.slug } }));
}

const activeLabel = computed(() => {
  const found = PANELS.find((p) => p.slug === props.activePanel);
  return found ? found.label : "Admin";
});

const isStub = computed(() => {
  const found = PANELS.find((p) => p.slug === props.activePanel);
  return !found || !found.enabled;
});
</script>

<template>
  <div class="grid h-[100dvh] w-full grid-cols-1 lg:grid-cols-[16rem_1fr] bg-background text-foreground">
    <!-- Mobile header with hamburger (visible <lg) -->
    <header class="flex h-12 items-center gap-2 border-b border-border px-3 lg:hidden">
      <button
        type="button"
        class="chat-icon-button hover:bg-muted"
        aria-label="Open admin navigation"
        @click="sidebarOpen = true"
      >
        <Menu class="h-5 w-5" />
      </button>
      <span class="type-pane-title flex-1 truncate">{{ activeLabel }}</span>
      <button
        type="button"
        class="chat-icon-button hover:bg-muted"
        aria-label="Back to chat"
        @click="emit('back')"
      >
        <ChevronLeft class="h-5 w-5" />
      </button>
    </header>

    <!-- Sidebar: pinned on lg+, slide-from-left drawer on <lg -->
    <aside
      class="hidden flex-col gap-3 border-r border-border bg-card/50 p-3 lg:flex lg:row-span-2"
      aria-label="Admin sections"
    >
      <div class="flex flex-col gap-1.5 px-2 pt-2">
        <button
          type="button"
          class="inline-flex items-center gap-1 self-start type-caption text-muted-foreground hover:text-foreground"
          aria-label="Back to chat"
          @click="emit('back')"
        >
          <ChevronLeft class="h-4 w-4" />
          <span>Back to chat</span>
        </button>
        <h1 class="type-pane-title">Admin</h1>
      </div>
      <nav>
        <ul class="flex flex-col gap-0.5">
          <li v-for="panel in PANELS" :key="panel.slug">
            <button
              type="button"
              class="flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 type-control text-left transition-colors"
              :class="[
                isActive(panel) ? 'bg-primary/10 text-primary font-semibold' : 'hover:bg-muted',
                !panel.enabled ? 'opacity-50 cursor-not-allowed' : '',
              ]"
              :aria-current="isActive(panel) ? 'page' : undefined"
              :aria-disabled="!panel.enabled"
              :disabled="!panel.enabled"
              :title="!panel.enabled ? 'Coming soon' : undefined"
              @click="onPanelClick(panel)"
            >
              <component :is="panel.icon" class="h-4 w-4 flex-shrink-0" aria-hidden="true" />
              <span class="truncate flex-1">{{ panel.label }}</span>
              <span v-if="!panel.enabled" class="rounded-full bg-muted px-2 py-0.5 type-caption">Soon</span>
            </button>
          </li>
        </ul>
      </nav>
    </aside>

    <!-- Mobile drawer overlay -->
    <Teleport to="body">
      <Transition name="drawer">
        <div
          v-if="sidebarOpen"
          class="z-modal fixed inset-0 lg:hidden"
          @keydown.esc="sidebarOpen = false"
        >
          <div
            class="absolute inset-0 bg-background/60 backdrop-blur-md"
            aria-hidden="true"
            @click="sidebarOpen = false"
          />
          <aside
            class="absolute left-0 top-0 h-[100dvh] w-[var(--chat-drawer-width)] glass-panel border-r border-border shadow-2xl flex flex-col"
            role="dialog"
            aria-modal="true"
            aria-label="Admin navigation drawer"
            @click.stop
          >
            <div class="h-12 flex items-center justify-between px-3 border-b border-border">
              <span class="type-pane-title">Admin</span>
              <button
                type="button"
                class="chat-icon-button hover:bg-muted"
                aria-label="Close admin navigation"
                @click="sidebarOpen = false"
              >
                <X class="w-4 h-4" />
              </button>
            </div>
            <nav class="flex-1 overflow-y-auto p-3">
              <ul class="flex flex-col gap-0.5">
                <li v-for="panel in PANELS" :key="panel.slug">
                  <button
                    type="button"
                    class="flex w-full items-center gap-2 rounded-md px-2.5 py-2 type-control text-left transition-colors"
                    :class="[
                      isActive(panel) ? 'bg-primary/10 text-primary font-semibold' : 'hover:bg-muted',
                      !panel.enabled ? 'opacity-50 cursor-not-allowed' : '',
                    ]"
                    :aria-current="isActive(panel) ? 'page' : undefined"
                    :aria-disabled="!panel.enabled"
                    :disabled="!panel.enabled"
                    @click="onPanelClick(panel)"
                  >
                    <component :is="panel.icon" class="h-4 w-4 flex-shrink-0" aria-hidden="true" />
                    <span class="truncate flex-1">{{ panel.label }}</span>
                    <span v-if="!panel.enabled" class="rounded-full bg-muted px-2 py-0.5 type-caption">Soon</span>
                  </button>
                </li>
              </ul>
            </nav>
          </aside>
        </div>
      </Transition>
    </Teleport>

    <!-- Main panel mount -->
    <main class="overflow-y-auto" role="main">
      <slot v-if="activePanel === 'users'" name="users" />
      <slot v-else-if="activePanel === 'spaces'" name="spaces" />
      <slot v-else-if="activePanel === 'channels'" name="channels" />
      <div v-else-if="isStub" class="mx-auto max-w-md p-6 text-center" role="status">
        <h2 class="type-pane-title mb-2">{{ activeLabel }}</h2>
        <p class="type-caption text-muted-foreground">
          This admin panel is in the IA but isn't implemented yet.
        </p>
      </div>
    </main>
  </div>
</template>

<style scoped>
.drawer-enter-active,
.drawer-leave-active {
  transition: opacity 150ms ease;
}
.drawer-enter-from,
.drawer-leave-to {
  opacity: 0;
}
</style>
