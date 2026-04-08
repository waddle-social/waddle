<script setup lang="ts">
import { ref, computed } from "vue";
import { ChevronDown, ChevronUp } from "lucide-vue-next";

const props = defineProps<{
  defaultServerUrl: string;
}>();

const emit = defineEmits<{
  login: [serverUrl: string];
}>();

const showCustomServer = ref(false);
const useCustom = ref(false);
const customUrl = ref("");

const displayDefault = computed(() => {
  try {
    return new URL(props.defaultServerUrl).hostname;
  } catch {
    return props.defaultServerUrl;
  }
});

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

function selectDefault() {
  useCustom.value = false;
}

function selectCustom() {
  useCustom.value = true;
}

function handleLogin() {
  emit("login", activeServerUrl.value);
}
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-6">
    <div class="w-full max-w-md border-2 border-foreground bg-background">
      <div class="p-6 border-b border-foreground">
        <h1 class="text-2xl font-mono font-bold uppercase tracking-wider">
          Waddle
        </h1>
        <p class="text-sm font-mono text-muted-foreground mt-2 leading-relaxed">
          Rooms, membership, and live chat in one compact client.
        </p>
      </div>

      <!-- Server selection -->
      <div class="p-6 border-b border-foreground space-y-4">
        <div class="flex items-center justify-between">
          <label class="text-xs font-mono uppercase tracking-widest text-muted-foreground">
            Homeserver
          </label>
          <button
            class="text-xs font-mono text-muted-foreground hover:text-foreground transition-colors flex items-center gap-1"
            @click="showCustomServer = !showCustomServer"
          >
            {{ showCustomServer ? "Hide" : "Edit" }}
            <component :is="showCustomServer ? ChevronUp : ChevronDown" class="w-3 h-3" />
          </button>
        </div>

        <div v-if="!showCustomServer" class="font-mono text-sm font-bold">
          {{ activeServerDisplay }}
        </div>

        <div v-else class="space-y-3">
          <!-- Default server option -->
          <label
            class="flex items-center gap-3 p-3 border cursor-pointer transition-colors"
            :class="!useCustom ? 'border-foreground bg-foreground text-background' : 'border-foreground hover:bg-muted'"
          >
            <input
              type="radio"
              name="server"
              :checked="!useCustom"
              class="sr-only"
              @change="selectDefault"
            />
            <span class="text-sm font-mono">{{ displayDefault }}</span>
            <span class="text-xs font-mono opacity-60 ml-auto">default</span>
          </label>

          <!-- Custom server option -->
          <label
            class="flex flex-col gap-2 p-3 border cursor-pointer transition-colors"
            :class="useCustom ? 'border-foreground bg-foreground text-background' : 'border-foreground hover:bg-muted'"
          >
            <div class="flex items-center gap-3">
              <input
                type="radio"
                name="server"
                :checked="useCustom"
                class="sr-only"
                @change="selectCustom"
              />
              <span class="text-sm font-mono">Other server</span>
            </div>
            <input
              v-if="useCustom"
              v-model="customUrl"
              type="url"
              placeholder="https://api.example.com"
              class="w-full font-mono text-sm px-3 py-2 border border-current bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-background"
              @focus="selectCustom"
            />
          </label>
        </div>
      </div>

      <!-- Login button -->
      <div class="p-6">
        <button
          class="w-full py-2 px-4 font-mono text-sm uppercase tracking-wider border border-foreground bg-foreground text-background hover:bg-foreground/90 transition-colors"
          @click="handleLogin"
        >
          Sign in via {{ activeServerDisplay }}
        </button>
      </div>
    </div>
  </div>
</template>
