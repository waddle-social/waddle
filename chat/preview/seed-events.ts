import { ref } from "vue";
import type { ChatAppController } from "@/shell/chat-app-controller";
import type { Attendee, CommunityEvent, PartStat } from "@/lib/xmpp-client";

/**
 * Preview fixtures for the events screens:
 *   events (month) | events-loading | events-error | events-none (no events)
 * Week view, empty filter, form and cancel sheet are reached by clicking.
 * `communityEvents.events` is a computed in the service, so the preview
 * swaps the property on the controller's plain object before navigating.
 */
export function seedEvents(c: ChatAppController, screen: string) {
  if (!screen.startsWith("events")) return;
  const now = new Date();
  const at = (days: number, hour: number, minute = 0) =>
    new Date(now.getFullYear(), now.getMonth(), now.getDate() + days, hour, minute).getTime();
  const day = (days: number) => {
    const d = new Date(now.getFullYear(), now.getMonth(), now.getDate() + days);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  };
  const rsvps = (going: number, maybe: number, cant: number, mine?: PartStat): Attendee[] => {
    const list: Attendee[] = [];
    const push = (n: number, partstat: PartStat) => {
      for (let i = 0; i < n; i++) list.push({ uri: `xmpp:p${list.length}@waddle.test`, partstat });
    };
    push(going, "ACCEPTED");
    push(maybe, "TENTATIVE");
    push(cant, "DECLINED");
    if (mine) list[0] = { uri: "xmpp:ada@waddle.test", partstat: mine };
    return list;
  };
  const inSoon = Date.now() + 40 * 60_000;
  const events: CommunityEvent[] = [
    {
      id: "reading", uid: "reading", summary: "XEP reading club",
      description: "This week: XEP-0313, Message Archive Management. Bring questions, leave with fewer.",
      location: "#xep-reading-club", organizer: "xmpp:ada@waddle.test",
      dtstart: { kind: "date-time", ms: inSoon }, dtend: { kind: "date-time", ms: inSoon + 3_600_000 },
      rrule: { freq: "WEEKLY", byDay: ["WE"] }, attendees: rsvps(6, 2, 1, "ACCEPTED"),
    },
    {
      id: "gonogo", uid: "gonogo", summary: "Release go/no-go",
      description: "Last check before tagging 0.9. Ten minutes, decisions only.",
      location: "#release-train", organizer: "xmpp:ada@waddle.test",
      dtstart: { kind: "date-time", ms: at(2, 18) }, dtend: { kind: "date-time", ms: at(2, 18, 30) },
      attendees: rsvps(4, 1, 0, "TENTATIVE"),
    },
    {
      id: "release", uid: "release", summary: "0.9 release day",
      description: "Tag, build, announce. Cocoa is on the house.", location: "Everywhere",
      organizer: "xmpp:jo@waddle.test",
      dtstart: { kind: "date", date: day(3) }, dtend: { kind: "date", date: day(4) },
      attendees: rsvps(9, 0, 0),
    },
    {
      id: "meetup", uid: "meetup", summary: "Community meetup",
      description: "Lightning talks, sticker swap and an install party for anyone who wants their own server.",
      location: "Community hall, room 2", organizer: "xmpp:priya@waddle.test",
      dtstart: { kind: "date-time", ms: at(6, 19) }, dtend: { kind: "date-time", ms: at(6, 21) },
      attendees: rsvps(5, 3, 2),
    },
    {
      id: "critique", uid: "critique", summary: "Design critique",
      description: "Bring one screen. Leave with three notes.", location: "#design",
      organizer: "xmpp:ada@waddle.test",
      dtstart: { kind: "date-time", ms: at(10, 15) }, dtend: { kind: "date-time", ms: at(10, 16) },
      rrule: { freq: "WEEKLY", interval: 2, byDay: ["TU"] }, attendees: rsvps(3, 2, 0),
    },
  ];
  const service = c.communityEvents as unknown as Record<string, unknown>;
  const list = screen === "events-loading" || screen === "events-none" ? [] : events;
  service.events = ref(list);
  service.isLoading = ref(screen === "events-loading");
  service.error = ref(screen === "events-error" ? "The calendar node did not answer." : null);
  service.findMaster = (uid: string) => list.find((event) => event.uid === uid) ?? null;
  c.openCommunitySurface("events");
}
