import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useCommunityEvents } from "../src/services/community-events";
import { dateTimeValue, dateValue } from "../src/lib/xmpp-client";
import type {
  BrowserXmppClient,
  CalendarDateValue,
  CommunityEvent,
  CommunityEventInput,
  Rrule,
} from "../src/lib/xmpp-client";

const COMMUNITY = "community.example.com";

type MockClient = BrowserXmppClient & {
  fetchCommunityEvents: ReturnType<typeof mock>;
  publishCommunityEvent: ReturnType<typeof mock>;
  updateCommunityEvent: ReturnType<typeof mock>;
  retractCommunityEvent: ReturnType<typeof mock>;
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
        ...(input.dtstart ? { dtstart: input.dtstart } : {}),
        ...(input.dtend ? { dtend: input.dtend } : {}),
        ...(input.rrule ? { rrule: input.rrule } : {}),
        ...(input.exdates ? { exdates: input.exdates } : {}),
      } satisfies CommunityEvent),
    ),
    updateCommunityEvent: mock((_jid: string, itemId: string, input: CommunityEventInput) =>
      Promise.resolve({
        id: itemId,
        uid: itemId,
        summary: input.summary,
        ...(input.description ? { description: input.description } : {}),
        ...(input.location ? { location: input.location } : {}),
        ...(input.organizer ? { organizer: input.organizer } : {}),
        ...(input.dtstart ? { dtstart: input.dtstart } : {}),
        ...(input.dtend ? { dtend: input.dtend } : {}),
        ...(input.rrule ? { rrule: input.rrule } : {}),
        ...(input.exdates ? { exdates: input.exdates } : {}),
      } satisfies CommunityEvent),
    ),
    retractCommunityEvent: mock(() => Promise.resolve()),
    rsvpCommunityEvent: mock(() => Promise.resolve()),
  } as unknown as MockClient;
}

function ms(value: CalendarDateValue | undefined): number | undefined {
  return value?.kind === "date-time" ? value.ms : undefined;
}

describe("useCommunityEvents", () => {
  test("refresh sorts ongoing and upcoming events before past events", async () => {
    const now = Date.now();
    const client = makeClient([
      {
        id: "ongoing",
        uid: "ongoing",
        summary: "Ongoing",
        dtstart: dateTimeValue(now - 60_000),
        dtend: dateTimeValue(now + 60_000),
      },
      { id: "soon", uid: "soon", summary: "Soon", dtstart: dateTimeValue(now + 60_000) },
      { id: "later", uid: "later", summary: "Later", dtstart: dateTimeValue(now + 86_400_000) },
      { id: "past", uid: "past", summary: "Past", dtstart: dateTimeValue(now - 86_400_000) },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    expect(events.events.value.map((e) => e.id)).toEqual(["ongoing", "soon", "later", "past"]);
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
      dtstart: dateTimeValue(Date.parse("2026-06-05T19:00:00Z")),
      rrule,
    });
    expect(posted?.rrule).toEqual(rrule);
  });

  test("refresh groups sibling RSVP items into the master event", async () => {
    const future = Date.now() + 86_400_000;
    const client = makeClient([
      { id: "evt-1", uid: "evt-1", summary: "Game Night", dtstart: dateTimeValue(future) },
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

  test("refresh preserves recurrence overrides while grouping sibling RSVP items", async () => {
    const start = Date.now() + 86_400_000;
    const recurrenceId = start + 7 * 86_400_000;
    const exdate = start + 14 * 86_400_000;
    const weekday = (["SU", "MO", "TU", "WE", "TH", "FR", "SA"] as const)[
      new Date(start).getUTCDay()
    ];
    const client = makeClient([
      {
        id: "evt-series",
        uid: "evt-series",
        summary: "Friday Game Night",
        dtstart: dateTimeValue(start),
        dtend: dateTimeValue(start + 60 * 60 * 1000),
        rrule: { freq: "WEEKLY", byDay: [weekday], count: 4 },
        exdates: [dateTimeValue(exdate)],
      },
      {
        id: "evt-series::override::1",
        uid: "evt-series",
        summary: "Special Round",
        recurrenceId: dateTimeValue(recurrenceId),
        dtstart: dateTimeValue(recurrenceId + 60 * 60 * 1000),
      },
      {
        id: "evt-series-rsvp-alice",
        uid: "evt-series",
        summary: "",
        attendees: [{ uri: "xmpp:alice@example.com", partstat: "ACCEPTED" }],
      },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();

    expect(events.calendarItems.value.length).toBe(1);
    const [series] = events.calendarItems.value;
    expect(series?.master.uid).toBe("evt-series");
    expect(series?.master.attendees?.[0]?.uri).toBe("xmpp:alice@example.com");
    expect(series?.master.exdates).toEqual([dateTimeValue(exdate)]);
    expect(series?.overrides.length).toBe(1);
    expect(series?.overrides[0]?.summary).toBe("Special Round");
    expect(ms(series?.overrides[0]?.recurrenceId)).toBe(recurrenceId);
    const expandedOverride = events.events.value.find((event) => ms(event.recurrenceId) === recurrenceId);
    expect(ms(expandedOverride?.dtstart)).toBe(recurrenceId + 60 * 60 * 1000);
    expect(ms(expandedOverride?.dtend)).toBe(recurrenceId + 2 * 60 * 60 * 1000);
  });

  test("rsvp publishes via wasm and optimistically folds a self-attendee in", async () => {
    const future = Date.now() + 86_400_000;
    const client = makeClient([
      { id: "evt-2", uid: "evt-2", summary: "Game Night", dtstart: dateTimeValue(future) },
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

  test("edit replaces the existing event by id and keeps it sorted", async () => {
    const future = Date.now() + 86_400_000;
    const client = makeClient([
      { id: "evt-a", uid: "evt-a", summary: "Original", dtstart: dateTimeValue(future) },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    const updated = await events.edit("evt-a", {
      summary: "Renamed",
      dtstart: dateTimeValue(future),
    });
    expect(updated?.summary).toBe("Renamed");
    expect(client.updateCommunityEvent).toHaveBeenCalledWith(
      COMMUNITY,
      "evt-a",
      expect.objectContaining({ summary: "Renamed" }),
    );
    expect(events.events.value.find((e) => e.id === "evt-a")?.summary).toBe("Renamed");
  });

  test("edit rejects empty summary", async () => {
    const client = makeClient([
      { id: "evt-a", uid: "evt-a", summary: "Original", dtstart: dateTimeValue(Date.now() + 1000) },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    const result = await events.edit("evt-a", { summary: "   " });
    expect(result).toBe(null);
    expect(client.updateCommunityEvent).not.toHaveBeenCalled();
  });

  test("cancel retracts and removes the event optimistically", async () => {
    const future = Date.now() + 86_400_000;
    const client = makeClient([
      { id: "evt-a", uid: "evt-a", summary: "Going away", dtstart: dateTimeValue(future) },
      { id: "evt-b", uid: "evt-b", summary: "Stays", dtstart: dateTimeValue(future + 1000) },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    const ok = await events.cancel("evt-a");
    expect(ok).toBe(true);
    expect(client.retractCommunityEvent).toHaveBeenCalledWith(COMMUNITY, "evt-a");
    expect(events.events.value.map((e) => e.id)).toEqual(["evt-b"]);
  });

  test("cancelInstance appends EXDATE on recurring master and republishes", async () => {
    const dtstart = Date.parse("2026-06-05T19:00:00Z");
    const skipInstance = Date.parse("2026-06-12T19:00:00Z");
    const client = makeClient([
      {
        id: "evt-series",
        uid: "evt-series",
        summary: "Friday Game Night",
        dtstart: dateTimeValue(dtstart),
        rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
      },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    const ok = await events.cancelInstance("evt-series", dateTimeValue(skipInstance));
    expect(ok).toBe(true);
    expect(client.updateCommunityEvent).toHaveBeenCalledTimes(1);
    const updateCall = client.updateCommunityEvent.mock.calls[0];
    expect(updateCall?.[0]).toBe(COMMUNITY);
    expect(updateCall?.[1]).toBe("evt-series");
    expect(updateCall?.[2]?.exdates).toEqual([dateTimeValue(skipInstance)]);
    expect(client.retractCommunityEvent).not.toHaveBeenCalled();
  });

  test("cancelInstance appends DATE EXDATEs for all-day recurring masters", async () => {
    const client = makeClient([
      {
        id: "evt-all-day",
        uid: "evt-all-day",
        summary: "Festival",
        dtstart: dateValue("2026-06-05"),
        dtend: dateValue("2026-06-06"),
        rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
      },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    const ok = await events.cancelInstance("evt-all-day", dateValue("2026-06-12"));
    expect(ok).toBe(true);
    const updateCall = client.updateCommunityEvent.mock.calls[0];
    expect(updateCall?.[2]?.exdates).toEqual([dateValue("2026-06-12")]);
    expect(updateCall?.[2]?.dtstart).toEqual(dateValue("2026-06-05"));
    expect(updateCall?.[2]?.dtend).toEqual(dateValue("2026-06-06"));
  });

  test("cancelInstance falls back to retract for non-recurring events", async () => {
    const future = Date.now() + 86_400_000;
    const client = makeClient([
      { id: "evt-once", uid: "evt-once", summary: "One-off", dtstart: dateTimeValue(future) },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    const ok = await events.cancelInstance("evt-once", dateTimeValue(future));
    expect(ok).toBe(true);
    expect(client.retractCommunityEvent).toHaveBeenCalledWith(COMMUNITY, "evt-once");
  });

  test("findMaster recovers the unexpanded master by uid", async () => {
    const dtstart = Date.parse("2026-06-05T19:00:00Z");
    const client = makeClient([
      {
        id: "evt-series",
        uid: "evt-series",
        summary: "Friday Game Night",
        dtstart: dateTimeValue(dtstart),
        rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
      },
    ]);
    const events = useCommunityEvents(ref<BrowserXmppClient | null>(client), {
      communityJid: ref<string | null>(COMMUNITY),
    });
    await events.refresh();
    // Sorted view contains synthetic-id instances.
    const instance = events.events.value[0]!;
    expect(instance.id).not.toBe(instance.uid);
    expect(instance.rrule).toBeUndefined();
    // findMaster routes back to the real pubsub item with RRULE intact.
    const master = events.findMaster(instance.uid);
    expect(master?.id).toBe("evt-series");
    expect(master?.rrule?.freq).toBe("WEEKLY");
  });
});
