export interface WaddleSummary {
  id: string;
  name: string;
  description: string | null;
  owner_user_id: string;
  icon_url: string | null;
  is_public: boolean;
  role?: "owner" | "admin" | "moderator" | "member" | null;
  created_at: string;
  updated_at: string | null;
}

export interface ChannelSummary {
  id: string;
  waddle_id: string;
  name: string;
  description: string | null;
  channel_type: string;
  position: number;
  is_default: boolean;
  created_at: string;
  updated_at: string | null;
}

export interface MemberSummary {
  user_id: string;
  username: string;
  role: "owner" | "admin" | "moderator" | "member";
  joined_at: string;
}

export interface UserSearchResult {
  id: string;
  username: string;
  display_name: string | null;
  avatar_url: string | null;
  jid: string;
}

export interface ChannelMessage {
  id: string;
  author: {
    user_id: string;
    username: string;
    display_name: string | null;
    avatar_url: string | null;
  };
  content: string | null;
  reply_to_id: string | null;
  thread_id: string | null;
  edited_at: string | null;
  created_at: string;
  expires_at: string | null;
}

interface ListWaddlesResponse {
  waddles: WaddleSummary[];
}

interface ListChannelsResponse {
  channels: ChannelSummary[];
}

interface ListMembersResponse {
  members: MemberSummary[];
}

interface ListMessagesResponse {
  messages: ChannelMessage[];
  next_cursor: string | null;
}

interface SearchUsersResponse {
  users: UserSearchResult[];
}

function trimTrailingSlash(value: string) {
  return value.endsWith("/") ? value.slice(0, -1) : value;
}

async function expectJson<T>(response: Response) {
  if (!response.ok) {
    let detail = "";

    try {
      const payload = (await response.json()) as {
        message?: unknown;
        error?: unknown;
      };
      detail =
        typeof payload.message === "string"
          ? `: ${payload.message}`
          : typeof payload.error === "string"
            ? `: ${payload.error}`
            : "";
    } catch {
      detail = "";
    }

    throw new Error(`Request failed (${response.status})${detail}`);
  }

  return (await response.json()) as T;
}

export class WaddleApi {
  private readonly baseUrl: string;
  private readonly sessionId: string;

  constructor(serverBaseUrl: string, sessionId: string) {
    this.baseUrl = trimTrailingSlash(serverBaseUrl);
    this.sessionId = sessionId;
  }

  private url(path: string, query?: Record<string, string | number | undefined>) {
    const url = new URL(path, `${this.baseUrl}/`);
    url.searchParams.set("session_id", this.sessionId);

    for (const [key, value] of Object.entries(query ?? {})) {
      if (value === undefined || value === "") {
        continue;
      }

      url.searchParams.set(key, String(value));
    }

    return url.toString();
  }

  private async json<T>(path: string, init?: RequestInit, query?: Record<string, string | number | undefined>) {
    const response = await fetch(this.url(path, query), {
      credentials: "include",
      ...init,
      headers: {
        "Content-Type": "application/json",
        ...(init?.headers ?? {}),
      },
    });

    return expectJson<T>(response);
  }

  async listWaddles() {
    const response = await fetch(this.url("/v1/waddles"), {
      credentials: "include",
    });

    return expectJson<ListWaddlesResponse>(response);
  }

  async createWaddle(input: {
    name: string;
    description?: string;
    is_public?: boolean;
  }) {
    return this.json<WaddleSummary>("/v1/waddles", {
      method: "POST",
      body: JSON.stringify(input),
    });
  }

  async updateWaddle(
    waddleId: string,
    input: {
      name?: string;
      description?: string;
      is_public?: boolean;
    },
  ) {
    return this.json<WaddleSummary>(`/v1/waddles/${waddleId}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    });
  }

  async deleteWaddle(waddleId: string) {
    const response = await fetch(this.url(`/v1/waddles/${waddleId}`), {
      method: "DELETE",
      credentials: "include",
    });

    if (!response.ok && response.status !== 204) {
      await expectJson<never>(response);
    }
  }

  async listChannels(waddleId: string) {
    const response = await fetch(this.url(`/v1/waddles/${waddleId}/channels`), {
      credentials: "include",
    });

    return expectJson<ListChannelsResponse>(response);
  }

  async createChannel(
    waddleId: string,
    input: {
      name: string;
      description?: string;
      channel_type?: string;
      position?: number;
    },
  ) {
    return this.json<ChannelSummary>(`/v1/waddles/${waddleId}/channels`, {
      method: "POST",
      body: JSON.stringify(input),
    });
  }

  async updateChannel(
    waddleId: string,
    channelId: string,
    input: {
      name?: string;
      description?: string;
      position?: number;
    },
  ) {
    return this.json<ChannelSummary>(
      `/v1/channels/${channelId}`,
      {
        method: "PATCH",
        body: JSON.stringify(input),
      },
      { waddle_id: waddleId },
    );
  }

  async deleteChannel(waddleId: string, channelId: string) {
    const response = await fetch(this.url(`/v1/channels/${channelId}`, { waddle_id: waddleId }), {
      method: "DELETE",
      credentials: "include",
    });

    if (!response.ok && response.status !== 204) {
      await expectJson<never>(response);
    }
  }

  async listMembers(waddleId: string) {
    const response = await fetch(this.url(`/v1/waddles/${waddleId}/members`), {
      credentials: "include",
    });

    return expectJson<ListMembersResponse>(response);
  }

  async addMember(
    waddleId: string,
    input: { user_id: string; role: "member" | "moderator" | "admin" },
  ) {
    return this.json<MemberSummary>(`/v1/waddles/${waddleId}/members`, {
      method: "POST",
      body: JSON.stringify(input),
    });
  }

  async updateMemberRole(
    waddleId: string,
    userId: string,
    role: "member" | "moderator" | "admin",
  ) {
    return this.json<MemberSummary>(
      `/v1/waddles/${waddleId}/members/${encodeURIComponent(userId)}`,
      {
        method: "PATCH",
        body: JSON.stringify({ role }),
      },
    );
  }

  async removeMember(waddleId: string, userId: string) {
    const response = await fetch(
      this.url(`/v1/waddles/${waddleId}/members/${encodeURIComponent(userId)}`),
      {
        method: "DELETE",
        credentials: "include",
      },
    );

    if (!response.ok && response.status !== 204) {
      await expectJson<never>(response);
    }
  }

  async searchUsers(query: string) {
    const response = await fetch(
      this.url("/v1/users/search", {
        query,
        limit: 8,
      }),
      {
        credentials: "include",
      },
    );

    return expectJson<SearchUsersResponse>(response);
  }

  async listMessages(waddleId: string, channelId: string, before?: string) {
    const response = await fetch(
      this.url(`/v1/waddles/${waddleId}/channels/${channelId}/messages`, {
        limit: 40,
        before,
      }),
      {
        credentials: "include",
      },
    );

    return expectJson<ListMessagesResponse>(response);
  }
}
