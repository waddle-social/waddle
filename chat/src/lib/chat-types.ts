export interface SpaceSummary {
  id: string;
  name: string;
  description?: string | null;
  icon_url?: string | null;
  is_public?: boolean;
  role?: "owner" | "admin" | "moderator" | "member" | null;
  created_at?: string;
  updated_at?: string | null;
}

export interface ChannelSummary {
  id: string;
  name: string;
  jid?: string;
  spaceId?: string;
  description?: string | null;
  channel_type?: string;
  position?: number;
  features?: string[];
  is_default?: boolean;
  created_at?: string;
  updated_at?: string | null;
}

export interface MemberSummary {
  jid: string;
  user_id?: string;
  username: string;
  avatar_url: string | null;
  role: "owner" | "admin" | "member" | "outcast" | "none";
  joined_at: string;
}

export interface UserSearchResult {
  id: string;
  username: string;
  display_name: string | null;
  avatar_url: string | null;
  jid: string;
}
