import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useCommunityEvents } from "../src/services/community-events";
import type { BrowserXmppClient, CommunityEvent, CommunityEventInput, Rrule } from "../src/lib/xmpp-client";

const COMMUNITY = "community.example.com";

type MockClient = BrowserXmppClient & {
  fetchCommunityEvents: ReturnType<typeof mock>;
  publishCommunityEvent: ReturnType<typeof mock>;
  rsvpCommunityEvent: ReturnType<typeof mock>;
};

function makeClient(events: CommunityEvent[] = []): MockClient {
  return {
    fetchCommunityEvents: mock(() => Promise.resolve(events)),
    publishCommunityEvent: mock((_jid: string, input: CommunityEventInput) =>
      Promise.resolve({
        id: "new-event",
        uid: "new-event",
        summary: input.summary,
        ...(input.description ? { description: input.description } : {}),
        ...(input.location ? { location: input.location } : {}),
        ...(input.organizer ? { organizer: input.organizer } : {}),
        ...(typeof input.dtstartMs === "number" ? { dtstartMs: input.dtstartMs } : {}),
        ...(typeof input.dtendMs === "number" ? { dtendMs: input.dtendMs } : {}),
        ...(input.rrule ? { rrule: input.rrule } : {}),
      } satisfies CommunityEvent),
    ),
    rsvpCommunityEvent: mock(() => Promise.resolve()),
  } as unknown as MockClient;
}

describe("useCommunityEvents", () => {
  test("refresh sorts upcoming events first by DTSTART", async () => {
    const now = Date.now();
    const client = makeClient([
      { id: "soon", uid: "soon", summary: "Soon", dtstartMs: now + 60_000 },
      { id: "later", uid: "later", summary: "Later", dtstartMs: now + 86_400_000 },
      { id: "past", uid: "past", summary: "Past", dtstartMs: now - 86_400_000 },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    expect(events.events.value.map((e) => e.id)).toEqual(["soon", "later", "past"]);
  });

  test("post appends locally and rejects empty summary", async () => {
    const client = makeClient([]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    const empty = await events.post({ summary: "   " });
    expect(empty).toBe(null);
    expect(client.publishCommunityEvent).not.toHaveBeenCalled();
    const posted = await events.post({ summary: "Launch day" });
    expect(posted?.id).toBe("new-event");
    expect(events.events.value.map((e) => e.id)).toContain("new-event");
  });

  test("RRULE round-trips through the composable", async () => {
    const client = makeClient([]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    const rrule: Rrule = { freq: "WEEKLY", byDay: ["FR"], count: 10 };
    const posted = await events.post({
      summary: "Friday Game Night",
      dtstartMs: Date.parse("2026-06-05T19:00:00Z"),
      rrule,
    });
    expect(posted?.rrule).toEqual(rrule);
  });

  test("refresh groups sibling RSVP items into the master event", async () => {
    const future = Date.now() + 86_400_000;
    const client = makeClient([
      { id: "evt-1", uid: "evt-1", summary: "Game Night", dtstartMs: future },
      {
        id: "evt-1-rsvp-alice",
        uid: "evt-1",
        summary: "",
        attendees: [{ uri: "xmpp:alice@example.com", partstat: "ACCEPTED" }],
      },
      {
        id: "evt-1-rsvp-bob",
        uid: "evt-1",
        summary: "",
        attendees: [{ uri: "xmpp:bob@example.com", partstat: "DECLINED" }],
      },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    // Only the master event survives; sibling RSVPs fold in as attendees.
    expect(events.events.value.map((e) => e.id)).toEqual(["evt-1"]);
    const attendees = events.events.value[0]?.attendees ?? [];
    expect(attendees.length).toBe(2);
    expect(attendees.find((a) => a.uri === "xmpp:alice@example.com")?.partstat).toBe("ACCEPTED");
    expect(attendees.find((a) => a.uri === "xmpp:bob@example.com")?.partstat).toBe("DECLINED");
  });

  test("rsvp publishes via wasm and optimistically folds a self-attendee in", async () => {
    const future = Date.now() + 86_400_000;
    const client = makeClient([
      { id: "evt-2", uid: "evt-2", summary: "Game Night", dtstartMs: future },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    const ok = await events.rsvp("evt-2", "alice", "alice@example.com", "ACCEPTED");
    expect(ok).toBe(true);
    expect(client.rsvpCommunityEvent).toHaveBeenCalledWith(
      COMMUNITY,
      "evt-2",
      "alice",
      "alice@example.com",
      "ACCEPTED",
    );
    const evt = events.events.value.find((e) => e.uid === "evt-2");
    expect(evt?.attendees?.[0]?.uri).toBe("xmpp:alice@example.com");
    expect(evt?.attendees?.[0]?.partstat).toBe("ACCEPTED");
  });

  test("rsvp returns false without communityJid / xmpp client", async () => {
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(null), {
      communityJid: ref<string | null>(null),
    });
    const ok = await events.rsvp("evt", "alice", "alice@example.com", "ACCEPTED");
    expect(ok).toBe(false);
  });
});
