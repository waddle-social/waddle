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

export function createServerAuth(serverBaseUrl: string) {
  const baseUrl = trimTrailingSlash(serverBaseUrl);

  return {
    loginUrl(nextUrl: string) {
      const url = new URL("/api/auth/start", `${baseUrl}/`);
      url.searchParams.set("provider", "colony");
      url.searchParams.set("flow", "browser");
      url.searchParams.set("next", nextUrl);
      return url.toString();
    },
    async getSession() {
      const response = await fetch(`${baseUrl}/api/auth/session`, {
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
    async logout() {
      const response = await fetch(`${baseUrl}/api/auth/logout`, {
        method: "POST",
        credentials: "include",
      });

      if (!response.ok && response.status !== 204) {
        throw new Error(`Failed to log out (${response.status})`);
      }
    },
  };
}
