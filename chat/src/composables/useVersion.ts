import { onScopeDispose, ref, watch, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";

const WEB_COMMIT_SHA = (import.meta.env.PUBLIC_COMMIT_SHA ?? "unknown").trim() || "unknown";
const SERVER_VERSION_RETRY_DELAY_MS = 5_000;
const SERVER_VERSION_MAX_ATTEMPTS = 3;

export interface ServerVersion {
  name?: string;
  version?: string;
  os?: string;
}

/**
 * Expose the web app's build-time commit SHA and the server's XEP-0092
 * software version so the sidebar can make deployment state visible.
 */
export function useVersion(xmppClient: Ref<BrowserXmppClient | null>) {
  const webCommitSha = ref(WEB_COMMIT_SHA);
  const serverVersion = ref<ServerVersion | null>(null);

  let fetchedForClient: BrowserXmppClient | null = null;
  let fetchGeneration = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const clearRetry = () => {
    if (retryTimer) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
  };

  const fetchServerVersion = (client: BrowserXmppClient, generation: number, attempt: number) => {
    void client
      .getServerVersion()
      .then((info) => {
        if (fetchedForClient !== client || fetchGeneration !== generation) return;
        serverVersion.value = info;
        if (!info && attempt < SERVER_VERSION_MAX_ATTEMPTS) {
          retryTimer = setTimeout(
            () => fetchServerVersion(client, generation, attempt + 1),
            SERVER_VERSION_RETRY_DELAY_MS,
          );
        }
      })
      .catch(() => {
        if (fetchedForClient !== client || fetchGeneration !== generation) return;
        serverVersion.value = null;
        if (attempt < SERVER_VERSION_MAX_ATTEMPTS) {
          retryTimer = setTimeout(
            () => fetchServerVersion(client, generation, attempt + 1),
            SERVER_VERSION_RETRY_DELAY_MS,
          );
        }
      });
  };

  watch(
    xmppClient,
    (client) => {
      if (!client) {
        clearRetry();
        fetchGeneration += 1;
        fetchedForClient = null;
        serverVersion.value = null;
        return;
      }
      if (client === fetchedForClient) return;
      clearRetry();
      fetchGeneration += 1;
      fetchedForClient = client;
      fetchServerVersion(client, fetchGeneration, 1);
    },
    { immediate: true },
  );

  onScopeDispose(() => {
    clearRetry();
    fetchGeneration += 1;
    fetchedForClient = null;
  });

  return { webCommitSha, serverVersion };
}

const RAW_SHA_PATTERN = /^[0-9a-f]{6,}$/i;

/** Extract a commit SHA from a raw XEP-0092 version or a legacy "0.1.0 (sha)" value. */
export function extractServerSha(version: ServerVersion | null): string | null {
  if (!version?.version) return null;
  const raw = version.version.trim();
  if (RAW_SHA_PATTERN.test(raw)) return raw;
  const match = raw.match(/\(([0-9a-f]{6,})\)/i);
  return match ? match[1] : null;
}

/**
 * Strip the "(sha)" suffix from a version string, leaving just the semantic
 * version. Used as a fallback when the SHA is missing or unparseable (e.g. a
 * build that couldn't stamp the git SHA and reports "0.1.0 (unknown)").
 */
export function extractServerShortVersion(version: ServerVersion | null): string | null {
  const raw = version?.version?.trim();
  if (!raw) return null;
  const stripped = raw.replace(/\s*\(.*\)\s*$/, "").trim();
  return stripped.length > 0 ? stripped : null;
}
