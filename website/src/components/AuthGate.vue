<template>
  <div class="min-h-screen w-full">
    <div v-if="status === 'checking'" class="flex h-full min-h-screen items-center justify-center bg-background text-foreground">
      <div class="flex flex-col items-center gap-2 text-sm font-mono uppercase tracking-[0.3em]">
        <span class="animate-pulse">Verifying Access</span>
        <span class="text-[10px] text-muted-foreground/80">Hold tight while we check your session</span>
      </div>
    </div>

    <div v-else-if="status === 'error'" class="flex h-full min-h-screen items-center justify-center bg-background text-foreground">
      <div class="flex flex-col items-center gap-4 text-center">
        <p class="text-sm font-mono text-muted-foreground max-w-xs">
          {{ errorMessage }}
        </p>
        <div class="flex gap-2">
          <button
            class="rounded border border-foreground px-4 py-2 text-xs font-mono uppercase tracking-widest transition hover:bg-foreground hover:text-background"
            type="button"
            @click="retry"
          >
            Try Again
          </button>
          <button
            class="rounded bg-foreground px-4 py-2 text-xs font-mono uppercase tracking-widest text-background transition hover:bg-foreground/90"
            type="button"
            @click="redirectToLogin(true)"
          >
            Go to Login
          </button>
        </div>
      </div>
    </div>

    <slot v-else />
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue';

const props = defineProps<{
  colonyBaseUrl: string;
}>();

type Status = 'checking' | 'ready' | 'error';

const status = ref<Status>('checking');
const errorMessage = ref<string>('We could not confirm your Colony session.');

const redirectToLogin = (immediate = false) => {
  if (typeof window === 'undefined') return;
  try {
    const loginUrl = new URL('/', props.colonyBaseUrl);
    loginUrl.searchParams.set('redirect', window.location.href);
    if (immediate) {
      window.location.href = loginUrl.toString();
    } else {
      window.location.replace(loginUrl.toString());
    }
  } catch (err) {
    console.error('AuthGate redirect failed', err);
    errorMessage.value = 'Something went wrong while redirecting to login.';
    status.value = 'error';
  }
};

const checkSession = async () => {
  if (typeof window === 'undefined') return;

  status.value = 'checking';

  try {
    const sessionUrl = new URL('/api/auth/session', props.colonyBaseUrl);
    const response = await fetch(sessionUrl.toString(), {
      credentials: 'include',
    });

    if (!response.ok) {
      throw new Error(`Session check failed with status ${response.status}`);
    }

    const result = await response.json();
    if (result?.authenticated) {
      status.value = 'ready';
      return;
    }

    redirectToLogin();
  } catch (error) {
    console.error('AuthGate session check error', error);
    errorMessage.value = 'We hit a snag verifying your session. Please try again.';
    status.value = 'error';
  }
};

const retry = () => {
  checkSession().catch((error) => {
    console.error('AuthGate retry failed', error);
    status.value = 'error';
  });
};

onMounted(() => {
  checkSession().catch((error) => {
    console.error('AuthGate initial check failed', error);
    status.value = 'error';
  });
});
</script>
