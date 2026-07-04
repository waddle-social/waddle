export type HeroTimeOfDay = "morning" | "day" | "evening" | "night";

export function heroTimeOfDayFor(date: Date): HeroTimeOfDay {
  const h = date.getHours();
  if (h >= 5 && h < 11) return "morning";
  if (h >= 11 && h < 17) return "day";
  if (h >= 17 && h < 22) return "evening";
  return "night";
}

export function heroGreetingFor(timeOfDay: HeroTimeOfDay): string {
  switch (timeOfDay) {
    case "morning": return "Good morning.";
    case "day":     return "Good afternoon.";
    case "evening": return "Good evening.";
    case "night":   return "Late one tonight.";
  }
}

export function heroQuietMessageFor(timeOfDay: HeroTimeOfDay): string {
  switch (timeOfDay) {
    case "morning": return "Everything's quiet. A good moment to start something.";
    case "day":     return "All caught up. The room is yours.";
    case "evening": return "All caught up. Maybe say hi to someone.";
    case "night":   return "Quiet night. Sleep well — or send a long-form note.";
  }
}

export function heroEyebrowFor(date: Date): string {
  return new Intl.DateTimeFormat(undefined, {
    weekday: "long",
    month: "short",
    day: "numeric",
  }).format(date);
}

export interface HeroSummary {
  totalUnread: number;
  totalMentions: number;
  totalThreadUnread: number;
  dmUnread: number;
  activeCalls: number;
  onlineFriends: number;
  hasUnread: boolean;
}

export interface HeroSummaryPart {
  count: number;
  label: string;
}

export function heroSummaryPartsFor(s: HeroSummary): HeroSummaryPart[] {
  const parts: HeroSummaryPart[] = [];
  if (s.totalMentions > 0) {
    parts.push({ count: s.totalMentions, label: s.totalMentions === 1 ? "mention" : "mentions" });
  }
  const unreadTotal = s.totalUnread + s.dmUnread;
  if (unreadTotal > 0) {
    parts.push({ count: unreadTotal, label: unreadTotal === 1 ? "unread message" : "unread messages" });
  }
  if (s.totalThreadUnread > 0) {
    parts.push({ count: s.totalThreadUnread, label: s.totalThreadUnread === 1 ? "thread reply" : "thread replies" });
  }
  if (s.activeCalls > 0) {
    parts.push({ count: s.activeCalls, label: s.activeCalls === 1 ? "active call" : "active calls" });
  }
  if (s.onlineFriends > 0) {
    parts.push({ count: s.onlineFriends, label: s.onlineFriends === 1 ? "friend online" : "friends online" });
  }
  return parts;
}
