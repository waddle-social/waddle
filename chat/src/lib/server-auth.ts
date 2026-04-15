export interface WaddleSession {
  session_id: string;
  user_id: string;
  username: string;
  avatar_url: string | null;
  xmpp_localpart: string;
  jid: string;
  xmpp_websocket_url: string;
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

export interface LoginUrlOptions {
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
        throw new Error(`Failed to load session (${response.status})`);
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

      if (!response.ok && response.status !== 204) {
        throw new Error(`Failed to log out (${response.status})`);
      }
    },
  };
}
