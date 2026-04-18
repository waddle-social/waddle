export type WaddleChannelType = "text" | "forum";

export function normalizeChannelType(value?: string | null): WaddleChannelType {
  return value === "forum" ? "forum" : "text";
}

export function isForumChannel(
  channel?: { channel_type?: string | null } | null,
): boolean {
  return normalizeChannelType(channel?.channel_type) === "forum";
}
