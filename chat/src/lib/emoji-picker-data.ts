export const COMMON_REACTIONS: readonly string[] = [
  "👍", "👎", "❤️", "🔥", "🎉", "👀", "😂", "😍",
  "🙏", "👏", "🙌", "💯", "✅", "❌", "⭐", "🚀",
  "🤔", "😢", "😮", "😡", "🤯", "🤝", "💪", "🧠",
  "👋", "😎", "🥳", "🙈", "💩", "🍕", "☕", "🐛",
];

export const RECENTS_STORAGE_KEY = "waddle:emoji-recents";

const RECENTS_CAP = 8;

export function loadRecents(): string[] {
  try {
    const raw = localStorage.getItem(RECENTS_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((v): v is string => typeof v === "string").slice(0, RECENTS_CAP);
  } catch {
    return [];
  }
}

export function pushRecent(emoji: string): string[] {
  const current = loadRecents();
  const next = [emoji, ...current.filter((e) => e !== emoji)].slice(0, RECENTS_CAP);
  try {
    localStorage.setItem(RECENTS_STORAGE_KEY, JSON.stringify(next));
  } catch {
    // ignore (SSR / privacy mode / quota)
  }
  return next;
}
