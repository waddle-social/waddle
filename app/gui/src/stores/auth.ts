import { ref, computed } from 'vue';
import { defineStore } from 'pinia';
import { useWaddle } from '../composables/useWaddle';
import { discoverWebSocketEndpoint } from '../utils/discover';

const STORAGE_KEY = 'waddle:auth';

interface PersistedAuth {
  jid: string;
  endpoint: string;
  /* password deliberately NOT persisted */
}

function loadPersisted(): PersistedAuth | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<PersistedAuth>;
    if (typeof parsed.jid === 'string' && typeof parsed.endpoint === 'string') {
      return { jid: parsed.jid, endpoint: parsed.endpoint };
    }
  } catch { /* corrupt data */ }
  return null;
}

function savePersisted(data: PersistedAuth): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
}

function clearPersisted(): void {
  localStorage.removeItem(STORAGE_KEY);
}

export const useAuthStore = defineStore('auth', () => {
  const jid = ref('');
  const endpoint = ref('');
  const authError = ref<string | null>(null);
  const loggingIn = ref(false);
  const isAuthenticated = ref(false);

  const nickname = computed(() => {
    const local = jid.value.split('@')[0];
    return local || jid.value || 'User';
  });

  // Restore previous session's JID/endpoint (but user must re-enter password)
  const persisted = loadPersisted();
  if (persisted) {
    jid.value = persisted.jid;
    endpoint.value = persisted.endpoint;
  }

  async function login(
    inputJid: string,
    password: string,
    inputEndpoint?: string,
  ): Promise<void> {
    authError.value = null;
    loggingIn.value = true;

    try {
      const bare = inputJid.split('/')[0] || inputJid;
      const [, domain] = bare.split('@', 2);
      if (!domain) {
        throw new Error('Invalid JID — expected user@domain');
      }

      // Discover or use provided endpoint
      let resolvedEndpoint = inputEndpoint?.trim() || '';
      if (!resolvedEndpoint) {
        resolvedEndpoint = await discoverWebSocketEndpoint(domain);
      }

      const waddle = useWaddle();
      await waddle.connect(inputJid, password, resolvedEndpoint);

      jid.value = inputJid;
      endpoint.value = resolvedEndpoint;
      isAuthenticated.value = true;

      savePersisted({ jid: inputJid, endpoint: resolvedEndpoint });
    } catch (err) {
      isAuthenticated.value = false;
      authError.value = err instanceof Error ? err.message : String(err);
      throw err;
    } finally {
      loggingIn.value = false;
    }
  }

  async function logout(): Promise<void> {
    try {
      const waddle = useWaddle();
      await waddle.disconnect();
    } catch { /* best-effort */ }

    isAuthenticated.value = false;
    jid.value = '';
    endpoint.value = '';
    authError.value = null;
    clearPersisted();
  }

  return {
    jid,
    endpoint,
    authError,
    loggingIn,
    isAuthenticated,
    nickname,
    login,
    logout,
  };
});
