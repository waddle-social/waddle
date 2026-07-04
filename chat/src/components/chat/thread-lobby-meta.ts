import type { MessageThreadEntry } from "@/channels/threads";
import type { OccupantPresence } from "@/lib/xmpp-client";

// Thread "lobby" metadata for the rich ThreadPanel header. Substitutes the
// bland "Thread > author" breadcrumb with: who started it, how many replies,
// who's been participating, and when activity last happened.

export function threadBreadcrumbLabels(
  threadStack: readonly string[],
  resolveEntry: (threadId: string) => MessageThreadEntry | undefined,
): string[] {
  return threadStack.map((id) => {
    const entry = resolveEntry(id);
    const body = entry?.root?.body?.trim() ?? "";
    return body.length > 0 ? body.slice(0, 40) : id.slice(0, 8);
  });
}

export function threadPreviewFor(entry: MessageThreadEntry | null): string {
  const body = entry?.root?.body?.trim() ?? "";
  if (!body) return "";
  return body.length > 96 ? `${body.slice(0, 95).trimEnd()}…` : body;
}

export interface ThreadParticipant {
  nick: string;
  avatarUrl?: string | null;
  presence: OccupantPresence;
}

/**
 * Materialise the set of unique authors contributing to a thread — ordered
 * by recency (most-recently-posted first). Include every participant — the
 * thread author themselves and the current user — so the avatar stack is
 * consistent regardless of who replies. Walk newest → oldest to bias the
 * visible avatars toward recent participants.
 */
export function threadParticipantsFor(
  entry: MessageThreadEntry | null,
  avatarUrlByAuthor: Record<string, string | null>,
  roomPresence: Record<string, OccupantPresence>,
): ThreadParticipant[] {
  if (!entry) return [];
  const seen = new Set<string>();
  const ordered: ThreadParticipant[] = [];
  const children = entry.directChildren;
  for (let i = children.length - 1; i >= 0; i--) {
    const c = children[i];
    if (!c) continue;
    if (seen.has(c.author)) continue;
    seen.add(c.author);
    ordered.push({
      nick: c.author,
      avatarUrl: avatarUrlByAuthor[c.author] ?? null,
      presence: roomPresence[c.author] ?? "offline",
    });
  }
  const root = entry.root;
  if (root && !seen.has(root.author)) {
    ordered.push({
      nick: root.author,
      avatarUrl: avatarUrlByAuthor[root.author] ?? null,
      presence: roomPresence[root.author] ?? "offline",
    });
  }
  return ordered;
}

const MAX_THREAD_AVATARS = 4;

export function visibleThreadParticipants(
  participants: readonly ThreadParticipant[],
): ThreadParticipant[] {
  return participants.slice(0, MAX_THREAD_AVATARS);
}

export function overflowThreadParticipantCount(
  participants: readonly ThreadParticipant[],
): number {
  return Math.max(0, participants.length - MAX_THREAD_AVATARS);
}

export function threadLastActivityFor(entry: MessageThreadEntry | null): string | null {
  if (!entry) return null;
  const last = entry.directChildren.at(-1) ?? entry.root;
  return last?.createdAt ?? null;
}

/**
 * Relative-time label for the header's "active … ago" pulse. Distinct from
 * `formatThreadRecency` (thread chips): this one says "yesterday" and keeps
 * counting days instead of falling back to a short date.
 */
export function formatThreadLastActivity(iso: string | null): string {
  if (!iso) return "";
  const ms = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(ms) || ms < 0) return "";
  const seconds = Math.floor(ms / 1000);
  if (seconds < 45) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 2) return "1 min ago";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 2) return "1 hour ago";
  if (hours < 24) return `${hours} hours ago`;
  const days = Math.floor(hours / 24);
  if (days < 2) return "yesterday";
  return `${days} days ago`;
}
