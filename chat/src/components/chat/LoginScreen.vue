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
      <div class="bg-card rounded-lg border border-border shadow-sm overflow-hidden">
        <!-- Header -->
        <div class="px-6 pt-6 pb-5">
          <div class="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center mb-4">
            <span class="text-xl">🐧</span>
          </div>
          <h1 class="text-[18px] font-semibold tracking-tight">
            Sign in to Waddle
          </h1>
          <p class="text-[13px] text-muted-foreground mt-1">
            Rooms, membership, and live chat.
          </p>
        </div>

        <!-- Server selection -->
        <div class="px-6 pb-5 space-y-3">
          <div class="flex items-center justify-between">
            <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
              Homeserver
            </label>
            <button
              class="text-[11px] text-muted-foreground hover:text-foreground transition-colors flex items-center gap-0.5"
              @click="showCustomServer = !showCustomServer"
            >
              {{ showCustomServer ? "Hide" : "Edit" }}
              <component :is="showCustomServer ? ChevronUp : ChevronDown" class="w-3 h-3" />
            </button>
          </div>

          <div v-if="!showCustomServer" class="text-[13px] font-medium flex items-center gap-1.5">
            <span class="w-1.5 h-1.5 rounded-full bg-success" />
            {{ activeServerDisplay }}
          </div>

          <div v-else class="space-y-1.5">
            <label
              class="flex items-center gap-2.5 p-2.5 rounded-md cursor-pointer transition-all border"
              :class="!useCustom ? 'border-primary bg-primary/5' : 'border-border hover:bg-muted'"
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
              class="flex flex-col gap-2 p-2.5 rounded-md cursor-pointer transition-all border"
              :class="useCustom ? 'border-primary bg-primary/5' : 'border-border hover:bg-muted'"
            >
              <div class="flex items-center gap-2.5">
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
                class="w-full text-[13px] px-2.5 py-1.5 rounded-md bg-background border border-border focus:outline-none focus:ring-1 focus:ring-ring"
                @focus="selectCustom"
              />
            </label>
          </div>
        </div>

        <!-- Providers / Login -->
        <div class="px-6 pb-6 space-y-2">
          <p
            v-if="errorMessage"
            class="rounded-md bg-destructive/10 border border-destructive/20 px-3 py-2 text-[13px] text-destructive"
          >
            {{ errorMessage }}
          </p>

          <template v-if="providers.length > 1">
            <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
              Sign in with
            </label>
            <button
              v-for="provider in providers"
              :key="provider.id"
              class="w-full py-2 px-3 text-[13px] font-medium rounded-md bg-primary text-primary-foreground hover:opacity-90 transition-all"
              @click="handleLogin(provider.id)"
            >
              {{ provider.display_name || provider.id }}
            </button>
          </template>

          <button
            v-else
            class="w-full py-2 px-3 text-[13px] font-medium rounded-md bg-primary text-primary-foreground hover:opacity-90 transition-all disabled:opacity-40"
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
