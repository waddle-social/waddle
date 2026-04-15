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
      <div class="glass-panel rounded-2xl border border-border shadow-2xl overflow-hidden">
        <!-- Header -->
        <div class="px-7 pt-8 pb-6">
          <div class="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center mb-5 shadow-[0_0_20px_var(--glow)]">
            <span class="text-2xl">🐧</span>
          </div>
          <h1 class="text-[22px] font-display font-bold tracking-tight">
            Sign in to Waddle
          </h1>
          <p class="text-[13px] text-muted-foreground mt-1.5">
            Rooms, membership, and live chat.
          </p>
        </div>

        <!-- Server selection -->
        <div class="px-7 pb-6 space-y-3">
          <div class="flex items-center justify-between">
            <label class="text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground">
              Homeserver
            </label>
            <button
              class="text-[11px] text-muted-foreground hover:text-primary transition-colors duration-200 flex items-center gap-0.5"
              @click="showCustomServer = !showCustomServer"
            >
              {{ showCustomServer ? "Hide" : "Edit" }}
              <component :is="showCustomServer ? ChevronUp : ChevronDown" class="w-3 h-3" />
            </button>
          </div>

          <div v-if="!showCustomServer" class="text-[13px] font-medium flex items-center gap-2">
            <span class="w-2 h-2 rounded-full bg-success shadow-[0_0_6px_var(--success)]" />
            {{ activeServerDisplay }}
          </div>

          <div v-else class="space-y-2">
            <label
              class="flex items-center gap-3 p-3 rounded-xl cursor-pointer transition-all duration-200"
              :class="!useCustom ? 'bg-primary/5 border border-primary/20' : 'border border-border hover:bg-muted'"
            >
              <input
                type="radio"
                name="server"
                :checked="!useCustom"
                class="sr-only"
                @change="selectDefault"
              />
              <span class="text-[13px] font-medium">{{ displayDefault }}</span>
              <span class="text-[11px] text-muted-foreground ml-auto">default</span>
            </label>

            <label
              class="flex flex-col gap-2 p-3 rounded-xl cursor-pointer transition-all duration-200"
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
                <span class="text-[13px] font-medium">Other server</span>
              </div>
              <input
                v-if="useCustom"
                v-model="customUrl"
                type="url"
                placeholder="https://server.example.com"
                class="w-full text-[13px] px-3 py-2 rounded-xl bg-muted focus:outline-none focus:ring-2 focus:ring-primary/20 transition-all duration-200"
                @focus="selectCustom"
              />
            </label>
          </div>
        </div>

        <!-- Providers / Login -->
        <div class="px-7 pb-7 space-y-2.5">
          <p
            v-if="errorMessage"
            class="rounded-xl bg-destructive/10 px-4 py-2.5 text-[13px] text-destructive"
          >
            {{ errorMessage }}
          </p>

          <template v-if="providers.length > 1">
            <label class="text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground">
              Sign in with
            </label>
            <button
              v-for="provider in providers"
              :key="provider.id"
              class="w-full py-2.5 px-4 text-[13px] font-semibold rounded-xl bg-primary text-primary-foreground hover:shadow-[0_0_20px_var(--glow-strong)] transition-all duration-300"
              @click="handleLogin(provider.id)"
            >
              {{ provider.display_name || provider.id }}
            </button>
          </template>

          <button
            v-else
            class="w-full py-2.5 px-4 text-[13px] font-semibold rounded-xl bg-primary text-primary-foreground hover:shadow-[0_0_20px_var(--glow-strong)] transition-all duration-300 disabled:opacity-30"
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
