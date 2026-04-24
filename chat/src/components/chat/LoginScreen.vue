<script setup lang="ts">
import { ref, computed, watch } from "vue";
import { ChevronDown, ChevronUp } from "lucide-vue-next";
import type { AuthProvider } from "@/lib/server-auth";

const props = defineProps<{
  defaultServerUrl: string;
  activeServerUrl: string;
  providers: AuthProvider[];
  errorMessage: string;
}>();

const emit = defineEmits<{
  login: [serverUrl: string, providerId?: string];
  fetchProviders: [serverUrl: string];
}>();

const showCustomServer = ref(false);
const useCustom = ref(false);
const customUrl = ref("");

function normalizeServerUrl(value: string) {
  return value.trim().replace(/\/$/, "");
}

const displayDefault = computed(() => {
  try {
    return new URL(props.defaultServerUrl).hostname;
  } catch {
    return props.defaultServerUrl;
  }
});

watch(
  () => props.activeServerUrl,
  (serverUrl) => {
    const normalizedActive = normalizeServerUrl(serverUrl);
    const normalizedDefault = normalizeServerUrl(props.defaultServerUrl);
    useCustom.value = normalizedActive !== normalizedDefault;
    customUrl.value = useCustom.value ? normalizedActive : "";
  },
  { immediate: true },
);

const activeServerUrl = computed(() =>
  useCustom.value && customUrl.value.trim()
    ? customUrl.value.trim()
    : props.defaultServerUrl,
);

const activeServerDisplay = computed(() => {
  if (useCustom.value && customUrl.value.trim()) {
    try {
      return new URL(customUrl.value.trim()).hostname;
    } catch {
      return customUrl.value.trim();
    }
  }
  return displayDefault.value;
});

const singleProviderLabel = computed(
  () => props.providers[0]?.display_name || props.providers[0]?.id || activeServerDisplay.value,
);

function selectDefault() {
  useCustom.value = false;
  customUrl.value = "";
  emit("fetchProviders", props.defaultServerUrl);
}

async function selectCustom() {
  useCustom.value = true;
}

let fetchTimer: ReturnType<typeof setTimeout> | null = null;

watch(customUrl, (url) => {
  if (!useCustom.value) return;
  if (fetchTimer) clearTimeout(fetchTimer);
  const trimmed = url.trim();
  if (!trimmed) return;

  fetchTimer = setTimeout(() => {
    emit("fetchProviders", trimmed);
  }, 500);
});

function handleLogin(providerId?: string) {
  emit("login", activeServerUrl.value, providerId);
}
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-6 bg-background">
    <div class="w-full max-w-sm animate-slide-up">
      <div class="glass-panel rounded-lg border border-border shadow-2xl overflow-hidden">
        <!-- Header -->
        <div class="flex flex-col gap-4 px-6 pt-7 pb-5">
          <div class="w-12 h-12 rounded-lg bg-primary/10 flex items-center justify-center shadow-[0_0_20px_var(--glow)]">
            <img
              src="/waddle-logo.svg"
              alt="Waddle"
              width="40"
              height="40"
              class="h-10 w-10 object-contain"
            />
          </div>
          <div class="chat-field-stack">
            <h1 class="type-display-title">
              Sign in to Waddle
            </h1>
            <p class="type-control text-muted-foreground">
              Rooms, membership, and live chat.
            </p>
          </div>
        </div>

        <!-- Server selection -->
        <div class="flex flex-col gap-3 px-6 pb-5">
          <div class="flex items-center justify-between">
            <label class="type-section-label text-muted-foreground">
              Homeserver
            </label>
            <button
              class="type-caption text-muted-foreground hover:text-primary transition-colors duration-200 flex items-center gap-0.5"
              type="button"
              aria-controls="login-server-options"
              :aria-expanded="showCustomServer"
              @click="showCustomServer = !showCustomServer"
            >
              {{ showCustomServer ? "Hide" : "Edit" }}
              <component :is="showCustomServer ? ChevronUp : ChevronDown" class="w-3 h-3" />
            </button>
          </div>

          <div v-if="!showCustomServer" class="type-control flex items-center gap-2">
            <span class="w-2 h-2 rounded-full bg-success shadow-[0_0_6px_var(--success)]" />
            {{ activeServerDisplay }}
          </div>

          <div v-else id="login-server-options" class="flex flex-col gap-2">
            <label
              class="flex items-center gap-3 p-3 rounded-lg cursor-pointer transition-all duration-200"
              :class="!useCustom ? 'bg-primary/5 border border-primary/20' : 'border border-border hover:bg-muted'"
            >
              <input
                type="radio"
                name="server"
                :checked="!useCustom"
                class="sr-only"
                @change="selectDefault"
              />
              <span class="type-control">{{ displayDefault }}</span>
              <span class="type-meta ml-auto text-muted-foreground">default</span>
            </label>

            <label
              class="flex flex-col gap-2 p-3 rounded-lg cursor-pointer transition-all duration-200"
              :class="useCustom ? 'bg-primary/5 border border-primary/20' : 'border border-border hover:bg-muted'"
            >
              <div class="flex items-center gap-3">
                <input
                  type="radio"
                  name="server"
                  :checked="useCustom"
                  class="sr-only"
                  @change="selectCustom"
                />
                <span class="type-control">Other server</span>
              </div>
              <input
                v-if="useCustom"
                v-model="customUrl"
                type="url"
                placeholder="https://server.example.com"
                aria-label="Custom homeserver URL"
                class="chat-field-control type-field"
                @focus="selectCustom"
              />
            </label>
          </div>
        </div>

        <!-- Providers / Login -->
        <div class="flex flex-col gap-2.5 px-6 pb-6">
          <p
            v-if="errorMessage"
            class="type-control rounded-lg bg-destructive/10 px-4 py-2.5 text-destructive"
            role="alert"
          >
            {{ errorMessage }}
          </p>

          <template v-if="providers.length > 1">
            <label class="type-section-label text-muted-foreground">
              Sign in with
            </label>
            <button
              v-for="provider in providers"
              :key="provider.id"
              class="chat-action-button chat-action-button--primary type-action w-full"
              type="button"
              @click="handleLogin(provider.id)"
            >
              {{ provider.display_name || provider.id }}
            </button>
          </template>

          <button
            v-else
            class="chat-action-button chat-action-button--primary type-action w-full disabled:opacity-30"
            type="button"
            :disabled="providers.length === 0"
            @click="handleLogin(providers[0]?.id)"
          >
            {{ providers.length === 0 ? "No sign-in methods found" : `Sign in with ${singleProviderLabel}` }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
