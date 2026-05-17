<script setup lang="ts">
import { computed } from "vue";
import { ChevronLeft, Users, FolderTree, ScrollText, BellRing, Settings as SettingsIcon } from "lucide-vue-next";
import { buildAdminPath, type AdminPanel } from "@/shell/navigation";

type StubPanel = Exclude<AdminPanel, "users">;

interface PanelDef {
  slug: AdminPanel;
  label: string;
  icon: typeof Users;
  /** V1 stubs are visually present so the IA is legible but cannot be navigated to. */
  enabled: boolean;
}

const props = defineProps<{
  /** Slug of the currently rendered panel. V1 only renders "users"; other slugs receive a stub view. */
  activePanel: AdminPanel;
}>();

const emit = defineEmits<{
  navigate: [path: string];
  back: [];
}>();

const PANELS: ReadonlyArray<PanelDef> = [
  { slug: "users", label: "Users", icon: Users, enabled: true },
  { slug: "spaces", label: "Spaces", icon: FolderTree, enabled: false },
  { slug: "audit", label: "Audit log", icon: ScrollText, enabled: false },
  { slug: "push-health", label: "Push health", icon: BellRing, enabled: false },
  { slug: "settings", label: "Settings", icon: SettingsIcon, enabled: false },
];

function isActive(panel: PanelDef): boolean {
  return panel.slug === props.activePanel;
}

function onPanelClick(panel: PanelDef): void {
  if (!panel.enabled) return;
  emit("navigate", buildAdminPath(panel.slug));
}

const stubLabel = computed(() => {
  const found = PANELS.find((p) => p.slug === props.activePanel);
  return found ? found.label : "Admin";
});
</script>

<template>
  <div class="admin-layout">
    <aside class="admin-sidebar" aria-label="Admin sections">
      <header class="admin-sidebar-header">
        <button
          type="button"
          class="admin-back-button"
          aria-label="Back to chat"
          @click="emit('back')"
        >
          <ChevronLeft :size="16" aria-hidden="true" />
          <span>Back to chat</span>
        </button>
        <h1 class="admin-title">Admin</h1>
      </header>
      <nav class="admin-nav">
        <ul>
          <li v-for="panel in PANELS" :key="panel.slug">
            <button
              type="button"
              class="admin-nav-item"
              :class="{
                'is-active': isActive(panel),
                'is-disabled': !panel.enabled,
              }"
              :aria-current="isActive(panel) ? 'page' : undefined"
              :aria-disabled="!panel.enabled"
              :disabled="!panel.enabled"
              :title="!panel.enabled ? 'Coming soon' : undefined"
              @click="onPanelClick(panel)"
            >
              <component :is="panel.icon" :size="16" aria-hidden="true" />
              <span>{{ panel.label }}</span>
              <span v-if="!panel.enabled" class="admin-soon-pill" aria-hidden="true">Soon</span>
            </button>
          </li>
        </ul>
      </nav>
    </aside>
    <main class="admin-main" role="main">
      <slot v-if="activePanel === 'users'" name="users" />
      <div v-else class="admin-stub" role="status">
        <h2>{{ stubLabel }}</h2>
        <p>
          This admin panel is part of the V1 frame but isn't implemented yet.
          The Users panel is the only fully wired surface for now.
        </p>
      </div>
    </main>
  </div>
</template>

<style scoped>
.admin-layout {
  display: grid;
  grid-template-columns: 16rem 1fr;
  height: 100dvh;
  background: var(--background, #f4f5f7);
  color: var(--foreground, #0c0d12);
}

.admin-sidebar {
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--border, rgba(15, 18, 25, 0.08));
  background: var(--sidebar, rgba(255, 255, 255, 0.72));
  padding: 1rem 0.75rem;
  gap: 0.75rem;
}

.admin-sidebar-header {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 0.25rem 0.5rem;
}

.admin-back-button {
  display: inline-flex;
  align-items: center;
  gap: 0.25rem;
  font-size: 0.85rem;
  color: var(--muted-foreground, rgba(15, 18, 25, 0.65));
  background: transparent;
  border: 0;
  cursor: pointer;
  padding: 0.25rem 0;
}

.admin-back-button:hover { color: var(--foreground, #0c0d12); }

.admin-title {
  font-size: 1.1rem;
  font-weight: 600;
  letter-spacing: -0.01em;
  margin: 0;
}

.admin-nav { padding-top: 0.5rem; }
.admin-nav ul { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.125rem; }

.admin-nav-item {
  width: 100%;
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.5rem 0.625rem;
  border-radius: 0.5rem;
  background: transparent;
  border: 0;
  color: var(--foreground, #0c0d12);
  font-size: 0.9rem;
  cursor: pointer;
  text-align: left;
  transition: background-color 120ms ease;
}

.admin-nav-item:hover:not(.is-disabled) { background: var(--accent, rgba(15, 18, 25, 0.06)); }

.admin-nav-item.is-active {
  background: var(--accent-strong, rgba(53, 99, 233, 0.12));
  color: var(--accent-foreground, #1f3aa3);
  font-weight: 600;
}

.admin-nav-item.is-disabled {
  cursor: not-allowed;
  color: var(--muted-foreground, rgba(15, 18, 25, 0.45));
}

.admin-soon-pill {
  margin-left: auto;
  font-size: 0.65rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  background: rgba(15, 18, 25, 0.06);
  padding: 0.05rem 0.4rem;
  border-radius: 999px;
}

.admin-main {
  overflow: auto;
  padding: 1.5rem 2rem;
}

.admin-stub {
  margin: 4rem auto;
  max-width: 32rem;
  text-align: center;
  color: var(--muted-foreground, rgba(15, 18, 25, 0.65));
}

.admin-stub h2 { font-size: 1.25rem; margin: 0 0 0.5rem 0; color: var(--foreground, #0c0d12); }
.admin-stub p { font-size: 0.9rem; line-height: 1.5; }
</style>
