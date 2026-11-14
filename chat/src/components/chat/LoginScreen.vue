<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount } from "vue";
import { ChevronDown, ChevronUp } from "lucide-vue-next";
import type { AuthProvider } from "@/lib/server-auth";
import { css, cx } from "styled-system/css";
import { btn, input, kicker } from "styled-system/recipes";
import spot from "@waddle/design/assets/spot-conversation.svg?url";

// App-SignIn board: illustration, title block, then one homeserver +
// providers card. Native radios stay (visually hidden) inside each option.
const page = css({ minHeight: "100dvh", display: "flex", alignItems: "center", justifyContent: "center", px: "4", py: "12", boxSizing: "border-box", bg: "bg", color: "fg" });
const column = css({ width: "100%", maxWidth: "384px", display: "flex", flexDirection: "column", alignItems: "stretch", gap: "5", animation: "wd-rise {durations.slow} {easings.out} backwards" });
const spotClass = css({ width: "120px", height: "120px", alignSelf: "center" });
const heading = css({ display: "flex", flexDirection: "column", gap: "2", textAlign: "center" });
const title = css({ margin: 0, font: "700 36px/1.1 {fonts.display}", letterSpacing: "-0.03em" });
const lead = css({ margin: 0, fontSize: "base", color: "fg.muted" });
const card = css({ display: "flex", flexDirection: "column", gap: "14px", p: "5", borderRadius: "panel", bg: "surface", border: "1px solid {colors.border}", "--ring-bg": "{colors.surface}" });
const row = css({ display: "flex", alignItems: "center", gap: "2" });
const kickerClass = kicker();
const grow = cx(kickerClass, css({ flex: 1 }));
const editButton = btn({ size: "sm" });
const serverLine = css({ display: "flex", alignItems: "center", gap: "2", mt: "-6px", font: "14px/1.3 {fonts.mono}" });
const liveDotClass = css({ width: "8px", height: "8px", borderRadius: "pill", bg: "live", flex: "none" });
const options = css({ display: "flex", flexDirection: "column", gap: "2" });
const option = css({
  display: "flex",
  flexDirection: "column",
  gap: "2",
  py: "3",
  px: "14px",
  borderRadius: "control",
  border: "1px solid {colors.border}",
  bg: "surface",
  fontSize: "sm",
  cursor: "pointer",
  transition: "background-color {durations.fast} {easings.out}, border-color {durations.fast} {easings.out}",
  _hover: { bgImage: "linear-gradient({colors.state.hover}, {colors.state.hover})" },
  "&:has(> div > input:checked)": { borderColor: "action", bg: "action.subtle" },
  "&:has(> div > input:focus-visible)": { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
  "& > div": { display: "flex", alignItems: "center", gap: "10px" },
});
const dot = css({
  width: "16px",
  height: "16px",
  flex: "none",
  boxSizing: "border-box",
  borderRadius: "pill",
  border: "1.5px solid {colors.border}",
  "label:has(> div > input:checked) &": { border: "5px solid {colors.action}" },
});
const srOnly = css({ srOnly: true });
const mono = css({ flex: 1, fontFamily: "mono" });
const urlInput = cx(input(), css({ width: "100%", font: "13px {fonts.mono}" }));
const divider = css({ height: "1px", bg: "border.soft" });
const errorBox = css({ py: "2.5", px: "3", borderRadius: "10px", bg: "color-mix(in srgb, {colors.danger} 12%, transparent)", border: "1px solid {colors.danger}", color: "danger.text", fontSize: "xs" });
const fullWidth = css({ width: "100%", height: "44px", justifyContent: "center" });
const primaryButton = cx(btn({ variant: "primary" }), fullWidth);
const secondaryButton = cx(btn(), fullWidth);

const props = defineProps<{
  defaultServerUrl: string;
  activeServerUrl: string;
  providers: AuthProvider[];
  errorMessage: string;
}>();

const emit = defineEmits<{
  login: [serverUrl: string, providerId?: string];
  fetchProviders: [serverUrl: string];
  invalidateProviders: [];
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
  useCustom.value ? customUrl.value.trim() : props.defaultServerUrl,
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
  if (fetchTimer) clearTimeout(fetchTimer);
  useCustom.value = false;
  customUrl.value = "";
  emit("fetchProviders", props.defaultServerUrl);
}

async function selectCustom() {
  if (useCustom.value) return;
  useCustom.value = true;
  emit("invalidateProviders");
}

let fetchTimer: ReturnType<typeof setTimeout> | null = null;

watch(customUrl, (url) => {
  if (!useCustom.value) return;
  if (fetchTimer) clearTimeout(fetchTimer);
  emit("invalidateProviders");
  const trimmed = url.trim();
  if (!trimmed) return;

  fetchTimer = setTimeout(() => {
    emit("fetchProviders", trimmed);
  }, 500);
});

onBeforeUnmount(() => {
  if (fetchTimer) clearTimeout(fetchTimer);
});

function handleLogin(providerId?: string) {
  emit("login", activeServerUrl.value, providerId);
}
</script>

<template>
  <div :class="page">
    <section :class="column">
      <img :src="spot" alt="Waddle" :class="spotClass" width="120" height="120" />
      <div :class="heading">
        <h1 :class="title">Sign in to Waddle</h1>
        <p :class="lead">Rooms, discussions, and the people in them.</p>
      </div>

      <div :class="card">
        <div :class="row">
          <span :class="grow">Homeserver</span>
          <button
            :class="editButton"
            type="button"
            aria-controls="login-server-options"
            :aria-expanded="showCustomServer"
            @click="showCustomServer = !showCustomServer"
          >
            {{ showCustomServer ? "Hide" : "Edit" }}
            <component :is="showCustomServer ? ChevronUp : ChevronDown" aria-hidden="true" />
          </button>
        </div>

        <span v-if="!showCustomServer" :class="serverLine">
          <span :class="liveDotClass" aria-hidden="true" />
          {{ activeServerDisplay }}
        </span>

        <div v-else id="login-server-options" role="radiogroup" aria-label="Homeserver" :class="options">
          <label :class="option">
            <div>
              <input type="radio" name="server" :checked="!useCustom" :class="srOnly" @change="selectDefault" />
              <span :class="dot" aria-hidden="true" />
              <span :class="mono">{{ displayDefault }}</span>
              <span :class="kickerClass">default</span>
            </div>
          </label>
          <label :class="option">
            <div>
              <input type="radio" name="server" :checked="useCustom" :class="srOnly" @change="selectCustom" />
              <span :class="dot" aria-hidden="true" />
              <span>Other server</span>
            </div>
          </label>
          <input
            v-if="useCustom"
            v-model="customUrl"
            type="url"
            placeholder="https://server.example.com"
            aria-label="Custom homeserver URL"
            :class="urlInput"
            @focus="selectCustom"
          />
        </div>

        <div :class="divider" />

        <p v-if="errorMessage" :class="errorBox" role="alert">{{ errorMessage }}</p>

        <template v-if="providers.length > 1">
          <span :class="kickerClass">Sign in with</span>
          <button
            v-for="(provider, index) in providers"
            :key="provider.id"
            :class="index === 0 ? primaryButton : secondaryButton"
            type="button"
            @click="handleLogin(provider.id)"
          >
            {{ provider.display_name || provider.id }}
          </button>
        </template>

        <button
          v-else
          :class="providers.length === 0 ? secondaryButton : primaryButton"
          type="button"
          :disabled="providers.length === 0"
          @click="handleLogin(providers[0]?.id)"
        >
          {{ providers.length === 0 ? "No sign-in methods found" : `Sign in with ${singleProviderLabel}` }}
        </button>
      </div>
    </section>
  </div>
</template>
