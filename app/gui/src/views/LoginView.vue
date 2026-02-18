<script setup lang="ts">
import { ref } from 'vue';
import { useRouter } from 'vue-router';
import { storeToRefs } from 'pinia';
import { useAuthStore } from '../stores/auth';

const router = useRouter();
const authStore = useAuthStore();
const { authError, loggingIn } = storeToRefs(authStore);

const jid = ref(authStore.jid || '');
const password = ref('');
const endpoint = ref(authStore.endpoint || '');
const showAdvanced = ref(false);

async function handleLogin(): Promise<void> {
  const inputJid = jid.value.trim();
  const inputPassword = password.value;
  const inputEndpoint = endpoint.value.trim() || undefined;

  if (!inputJid || !inputPassword) return;

  try {
    await authStore.login(inputJid, inputPassword, inputEndpoint);
    await router.replace('/');
  } catch {
    // Error is already set in authStore.authError
  }
}

function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Enter') {
    event.preventDefault();
    void handleLogin();
  }
}
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-background px-4">
    <div class="w-full max-w-sm">
      <!-- Logo / branding -->
      <div class="mb-8 text-center">
        <div class="mx-auto mb-4 flex h-20 w-20 items-center justify-center rounded-full bg-surface text-4xl">
          🐧
        </div>
        <h1 class="text-2xl font-bold text-foreground">Welcome to Waddle</h1>
        <p class="mt-1 text-sm text-muted">Sign in with your XMPP account</p>
      </div>

      <!-- Login form -->
      <form class="space-y-4" @submit.prevent="handleLogin">
        <!-- JID -->
        <div>
          <label for="jid" class="mb-1 block text-xs font-semibold uppercase tracking-wide text-muted">
            JID (user@domain)
          </label>
          <input
            id="jid"
            v-model="jid"
            type="text"
            placeholder="alice@example.com"
            autocomplete="username"
            required
            class="w-full rounded-lg bg-surface px-4 py-3 text-sm text-foreground placeholder-muted outline-none focus:ring-2 focus:ring-accent"
            @keydown="handleKeydown"
          />
        </div>

        <!-- Password -->
        <div>
          <label for="password" class="mb-1 block text-xs font-semibold uppercase tracking-wide text-muted">
            Password
          </label>
          <input
            id="password"
            v-model="password"
            type="password"
            placeholder="••••••••"
            autocomplete="current-password"
            required
            class="w-full rounded-lg bg-surface px-4 py-3 text-sm text-foreground placeholder-muted outline-none focus:ring-2 focus:ring-accent"
            @keydown="handleKeydown"
          />
        </div>

        <!-- Advanced toggle -->
        <button
          type="button"
          class="flex items-center gap-1 text-xs text-muted transition-colors hover:text-foreground"
          @click="showAdvanced = !showAdvanced"
        >
          <svg
            class="h-3 w-3 transition-transform"
            :class="showAdvanced ? 'rotate-90' : ''"
            fill="currentColor"
            viewBox="0 0 20 20"
          >
            <path
              fill-rule="evenodd"
              d="M7.21 14.77a.75.75 0 01.02-1.06L11.168 10 7.23 6.29a.75.75 0 111.04-1.08l4.5 4.25a.75.75 0 010 1.08l-4.5 4.25a.75.75 0 01-1.06-.02z"
              clip-rule="evenodd"
            />
          </svg>
          Advanced — set server endpoint
        </button>

        <!-- Endpoint (advanced) -->
        <div v-if="showAdvanced">
          <label for="endpoint" class="mb-1 block text-xs font-semibold uppercase tracking-wide text-muted">
            WebSocket Endpoint (optional)
          </label>
          <input
            id="endpoint"
            v-model="endpoint"
            type="text"
            placeholder="wss://example.com:5281/xmpp-websocket"
            autocomplete="off"
            class="w-full rounded-lg bg-surface px-4 py-3 text-sm text-foreground placeholder-muted outline-none focus:ring-2 focus:ring-accent"
            @keydown="handleKeydown"
          />
          <p class="mt-1 text-xs text-muted">
            Leave empty to auto-discover via XEP-0156
          </p>
        </div>

        <!-- Error -->
        <div v-if="authError" class="rounded-lg bg-danger/20 px-4 py-3 text-sm text-danger">
          {{ authError }}
        </div>

        <!-- Submit -->
        <button
          type="submit"
          class="w-full rounded-lg bg-accent py-3 text-sm font-semibold text-white transition-colors hover:bg-accent/80 disabled:opacity-50"
          :disabled="loggingIn || !jid.trim() || !password"
        >
          {{ loggingIn ? 'Connecting…' : 'Sign In' }}
        </button>
      </form>
    </div>
  </div>
</template>
