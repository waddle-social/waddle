import { reportError } from "@/lib/telemetry";

export interface WaddleSession {
  session_id: string;
  username: string;
  avatar_url: string | null;
  xmpp_localpart: string;
  jid: string;
  xmpp_websocket_url: string;
  link_preview_media_origin?: string;
  is_expired: boolean;
  expires_at: string | null;
}

function trimTrailingSlash(value: string) {
  return value.endsWith("/") ? value.slice(0, -1) : value;
}

export interface AuthProvider {
  id: string;
  kind: string;
  display_name?: string;
}

interface LoginUrlOptions {
  sessionTransport?: "cookie" | "fragment";
}

export function createServerAuth(serverBaseUrl: string) {
  const baseUrl = trimTrailingSlash(serverBaseUrl);

  return {
    async getProviders(): Promise<AuthProvider[]> {
      const response = await fetch(`${baseUrl}/api/auth/providers`, {
        credentials: "include",
      });

      if (!response.ok) {
        return [];
      }

      const data = await response.json();
      return Array.isArray(data) ? (data as AuthProvider[]) : [];
    },

    loginUrl(nextUrl: string, providerId: string, options?: LoginUrlOptions) {
      const url = new URL("/api/auth/start", `${baseUrl}/`);
      url.searchParams.set("provider", providerId);
      url.searchParams.set("flow", "browser");
      url.searchParams.set("next", nextUrl);
      if (options?.sessionTransport) {
        url.searchParams.set("session_transport", options.sessionTransport);
      }
      return url.toString();
    },
    async getSession(sessionId?: string) {
      const url = new URL("/api/auth/session", `${baseUrl}/`);
      if (sessionId) {
        url.searchParams.set("session_id", sessionId);
      }

      const response = await fetch(url.toString(), {
        credentials: "include",
      });

      if (response.status === 401 || response.status === 404) {
        return null;
      }

      if (!response.ok) {
        const err = new Error(`Failed to load session (${response.status})`);
        reportError({
          kind: "http.fetch",
          operation: "auth-session-get",
          status: response.status,
          recoverable: false,
        });
        throw err;
      }

      return (await response.json()) as WaddleSession;
    },
    async logout(sessionId?: string) {
      const init: RequestInit = {
        method: "POST",
        credentials: "include",
        headers: {
          "Content-Type": "application/json",
        },
      };
      if (sessionId) {
        init.body = JSON.stringify({ session_id: sessionId });
      }

      const response = await fetch(`${baseUrl}/api/auth/logout`, init);

      // 204 is the success shape. 401/404 both mean "nothing to log
      // out" (session already expired / never existed) — mirror the
      // getSession() treatment and stay silent, no telemetry noise.
      if (response.ok || response.status === 204) return;
      if (response.status === 401 || response.status === 404) return;

      const err = new Error(`Failed to log out (${response.status})`);
      reportError({
        kind: "http.fetch",
        operation: "auth-logout-post",
        status: response.status,
        recoverable: true,
      });
      throw err;
    },
  };
}
