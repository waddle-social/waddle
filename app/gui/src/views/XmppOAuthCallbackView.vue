<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { useAuthStore } from '../stores/auth';

const route = useRoute();
const router = useRouter();
const authStore = useAuthStore();

const status = ref<'processing' | 'error'>('processing');
const message = ref('Completing OAuth sign-in…');

onMounted(async () => {
  try {
    const callbackError = route.query.error;
    const callbackDescription = route.query.error_description;

    if (typeof callbackError === 'string') {
      const details = typeof callbackDescription === 'string'
        ? callbackDescription
        : callbackError;
      throw new Error(details);
    }

    const code = route.query.code;
    const state = route.query.state;
    if (typeof code !== 'string' || typeof state !== 'string') {
      throw new Error('Missing OAuth callback parameters');
    }

    await authStore.completeOAuthLogin(code, state);
    await router.replace('/');
  } catch (err) {
    status.value = 'error';
    message.value = err instanceof Error ? err.message : String(err);
  }
});

async function backToLogin(): Promise<void> {
  await router.replace({ name: 'login' });
}
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-background px-4">
    <div class="w-full max-w-sm rounded-lg bg-surface p-6 text-center">
      <h1 class="mb-2 text-xl font-bold text-foreground">
        {{ status === 'processing' ? 'Signing in…' : 'OAuth sign-in failed' }}
      </h1>

      <p
        class="text-sm"
        :class="status === 'processing' ? 'text-muted' : 'text-danger'"
      >
        {{ message }}
      </p>

      <button
        v-if="status === 'error'"
        type="button"
        class="mt-4 w-full rounded-lg bg-accent py-2 text-sm font-semibold text-white transition-colors hover:bg-accent/80"
        @click="backToLogin"
      >
        Back to login
      </button>
    </div>
  </div>
</template>
