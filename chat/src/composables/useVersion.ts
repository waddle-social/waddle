import { ref, watch, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";

const WEB_COMMIT_SHA = (import.meta.env.PUBLIC_COMMIT_SHA ?? "unknown").trim() || "unknown";

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

  watch(
    xmppClient,
    (client) => {
      if (!client) {
        fetchedForClient = null;
        serverVersion.value = null;
        return;
      }
      if (client === fetchedForClient) return;
      fetchedForClient = client;
      void client.getServerVersion().then((info) => {
        if (fetchedForClient === client) {
          serverVersion.value = info;
        }
      });
    },
    { immediate: true },
  );

  return { webCommitSha, serverVersion };
}

/** Extract a short commit SHA from a server version string like "0.1.0 (abc123def456)". */
export function extractServerSha(version: ServerVersion | null): string | null {
  if (!version?.version) return null;
  const match = version.version.match(/\(([0-9a-f]{6,})\)/i);
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
