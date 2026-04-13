export interface WaddleSession {
  session_id: string;
  user_id: string;
  username: string;
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

export interface ServerInfo {
  native_auth_available: boolean;
  registration_available?: boolean;
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

    async getServerInfo(): Promise<ServerInfo | null> {
      const response = await fetch(`${baseUrl}/api/v1/server-info`, {
        credentials: "include",
      });

      if (!response.ok) {
        return null;
      }

      return (await response.json()) as ServerInfo;
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
    async loginNative(username: string, password: string) {
      return nativeAuthRequest("/api/auth/native/login", { username, password });
    },
    async registerNative(username: string, password: string) {
      return nativeAuthRequest("/api/auth/native/register", { username, password });
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

  async function nativeAuthRequest(path: string, body: { username: string; password: string }) {
    const response = await fetch(`${baseUrl}${path}`, {
      method: "POST",
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });

    if (!response.ok) {
      let message = `Authentication failed (${response.status})`;
      try {
        const payload = (await response.json()) as { message?: unknown; error?: unknown };
        if (typeof payload.message === "string") {
          message = payload.message;
        } else if (typeof payload.error === "string") {
          message = payload.error;
        }
      } catch {
        // keep fallback message
      }
      throw new Error(message);
    }

    return (await response.json()) as WaddleSession;
  }
}
