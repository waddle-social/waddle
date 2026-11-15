import { describe, expect, mock, test } from "bun:test";
import { reactive, type Ref } from "vue";
import { eventPlanningCalendar, newPlanningItem, planningInstant, planningLocal, safePlanningUrl, type PlanningItem } from "../src/lib/xmpp/event-planning";
import { renderVueComponent, setupVueComponent } from "./helpers/render-vue-sfc";
const ROOM = "event-london@muc.example.com";
const SELF = "alice@example.com";
const metadata: PlanningItem = { ...newPlanningItem("event", "London maintainers", "event"), author: SELF, timezone: "Europe/London" };
function client() {
  return {
    ensureJoined: mock(async () => {}),
    fetchUserBookmarks: mock(async () => []),
    fetchEventPlanning: mock(async (_room: string): Promise<PlanningItem[]> => [metadata]),
    publishEventPlanning: mock(async (_room: string, item: PlanningItem) => ({ ...item, author: SELF })),
    retractEventPlanning: mock(async () => {}), createEventRoom: mock(async () => ROOM),
    listRoomMembers: mock(async () => [{ jid: SELF, affiliation: "owner", username: "alice", joined_at: "" }]),
    setEventRoomAffiliation: mock(async () => {}),
    sendGroupMessage: mock(async () => ({ id: "discussion", state: "sending" })),
    sendDirectMessage: mock(async () => ({ id: "invite", state: "sending" })),
    setRoomNotificationMode: mock(async () => ({ kind: "ok" })),
  };
}
interface Panel {
  selectRoom: (room: string) => Promise<void>; refresh: () => Promise<void>;
  createEvent: () => Promise<void>; saveEditor: () => Promise<void>;
  discuss: (item: PlanningItem) => Promise<void>;
  startEdit: (item: PlanningItem) => void; invite: () => Promise<void>;
  rsvp: (status: string) => Promise<void>; vote: (poll: PlanningItem, option: string) => Promise<void>;
  items: Ref<PlanningItem[]>; editor: Ref<PlanningItem | null>; createName: Ref<string>;
  selectedRoom: Ref<string>; inviteJid: Ref<string>; inviteRole: Ref<string>; error: Ref<string | null>;
}
async function panel(api = client()) {
  const props = reactive({ client: api, selfJid: SELF });
  const state = await setupVueComponent("../src/components/community/EventPlanningPanel.vue", props, import.meta.url) as unknown as Panel;
  return { state, api, props };
}
describe("event planning dates and export", () => {
  test("London wall times resolve in winter and summer independently of the viewer's zone", () => {
    expect(planningInstant("2027-02-12T09:00", "Europe/London")).toBe("2027-02-12T09:00:00.000Z");
    expect(planningInstant("2027-07-12T09:00", "Europe/London")).toBe("2027-07-12T08:00:00.000Z");
    expect(planningLocal("2027-07-12T08:00:00Z", "Europe/London")).toBe("2027-07-12T09:00");
    expect(planningInstant("", "Europe/London")).toBeUndefined();
  });
  test("rejects nonexistent and ambiguous clock-change times", () => {
    expect(() => planningInstant("2027-03-28T01:30", "Europe/London")).toThrow("does not exist");
    expect(() => planningInstant("2027-10-31T01:30", "Europe/London")).toThrow("occurs twice");
    expect(() => planningInstant("2027-02-30T09:00", "Europe/London")).toThrow();
    expect(() => planningInstant("2027-02-12T09:00", "Nowhere/Missing")).toThrow("valid time zone");
  });
  test("exports only scheduled event/itinerary objects with escaped and folded UTF-8", () => {
    const itinerary = { ...newPlanningItem("itinerary", "Lunch, plans;\nLondon"), starts_at: "2027-02-12T12:00:00Z", ends_at: "2027-02-12T13:00:00Z", description: "🦆".repeat(50) };
    const calendar = eventPlanningCalendar([metadata, itinerary, newPlanningItem("travel", "Private arrival")], ROOM, new Date("2026-11-15T00:00:00Z"));
    expect(calendar).toContain("DTSTART:20270212T120000Z");
    expect(calendar).toContain("SUMMARY:Lunch\\, plans\\;\\nLondon");
    expect(calendar).not.toContain("Private arrival"); expect(calendar).not.toContain("London maintainers");
    for (const line of calendar.split("\r\n")) expect(new TextEncoder().encode(line).length).toBeLessThanOrEqual(75);
    expect(calendar.match(/BEGIN:VEVENT/g)).toHaveLength(1);
  });
  test("rejects executable and invalid saved link schemes", () => {
    expect(safePlanningUrl("javascript:alert(1)")).toBeNull();
    expect(safePlanningUrl("data:text/html,hello")).toBeNull(); expect(safePlanningUrl("not a url")).toBeNull();
    expect(safePlanningUrl("https://example.com/hotel")).toBe("https://example.com/hotel");
  });
});
describe("event workspace interactions", () => {
  test("renders a private planning entry point", async () => {
    const html = await renderVueComponent("../src/components/community/EventPlanningPanel.vue", { client: client(), selfJid: SELF }, import.meta.url);
    expect(html).toContain("Events &amp; trips"); expect(html).toContain("Plan an event"); expect(html).toContain("Only invited members");
  });
  test("late responses cannot overwrite a newly selected event", async () => {
    const { api, state } = await panel(); let resolveFirst!: (items: PlanningItem[]) => void;
    api.fetchEventPlanning.mockImplementation((room) => room === ROOM ? new Promise((resolve) => { resolveFirst = resolve; }) : Promise.resolve([{ ...metadata, title: "Paris" }]));
    const first = state.selectRoom(ROOM); await Promise.resolve(); await state.selectRoom("event-paris@muc.example.com"); resolveFirst([metadata]); await first;
    expect(state.items.value[0]?.title).toBe("Paris");
  });
  test("failed metadata publication retries in the same private room", async () => {
    const { api, state } = await panel(); api.publishEventPlanning.mockRejectedValueOnce(new Error("Disconnected")); state.createName.value = "London";
    await state.createEvent(); expect(api.createEventRoom).toHaveBeenCalledWith("London", true); expect(state.selectedRoom.value).toBe(ROOM);
    expect(state.editor.value?.id).toBe("event"); expect(state.error.value).toContain("Disconnected"); await state.saveEditor();
    expect(api.createEventRoom).toHaveBeenCalledTimes(1); expect(api.publishEventPlanning).toHaveBeenCalledTimes(2); expect(state.editor.value).toBeNull();
  });
  test("invites grant event access before notifying the person", async () => {
    const { api, state } = await panel(); await state.selectRoom(ROOM); state.inviteJid.value = "bob@example.com"; state.inviteRole.value = "admin"; await state.invite();
    expect(api.setEventRoomAffiliation).toHaveBeenCalledWith(ROOM, "bob@example.com", "admin");
    expect(api.sendDirectMessage).toHaveBeenCalledWith("bob@example.com", expect.stringContaining(ROOM));
    api.setEventRoomAffiliation.mockRejectedValueOnce(new Error("Forbidden")); state.inviteJid.value = "eve@example.com"; await state.invite();
    expect(api.sendDirectMessage).toHaveBeenCalledTimes(1); expect(state.error.value).toBe("Forbidden");
  });
  test("RSVP and votes use self-scoped identifiers", async () => {
    const { api, state } = await panel(); await state.selectRoom(ROOM); await state.rsvp("going");
    expect(api.publishEventPlanning).toHaveBeenLastCalledWith(ROOM, expect.objectContaining({ id: `attendance:${SELF}`, attendance: "going" }));
    await state.vote(newPlanningItem("poll", "Dinner?", "dinner"), "b");
    expect(api.publishEventPlanning).toHaveBeenLastCalledWith(ROOM, expect.objectContaining({ id: `vote:dinner:${SELF}`, subject: "dinner", selections: ["b"] }));
  });
  test("failed refresh drops previously visible private details", async () => {
    const { api, state } = await panel(); await state.selectRoom(ROOM); expect(state.items.value).toHaveLength(1);
    api.fetchEventPlanning.mockRejectedValueOnce(new Error("Forbidden")); await state.refresh();
    expect(state.items.value).toHaveLength(0); expect(state.error.value).toBe("Forbidden");
  });
  test("planning objects share a stable discussion thread in the event conversation", async () => {
    const { api, state } = await panel(); await state.selectRoom(ROOM);
    const hotel = { ...newPlanningItem("link", "Hotel", "hotel-link"), url: "https://example.com/hotel" };
    await state.discuss(hotel);
    expect(api.sendGroupMessage).toHaveBeenCalledWith("", ROOM, "Hotel\nhttps://example.com/hotel", { threadId: "hotel-link" });
  });
  test("clearing optional typed links and assignees publishes null", async () => {
    const { api, state } = await panel(); await state.selectRoom(ROOM);
    state.startEdit({ ...newPlanningItem("task", "Book dinner"), author: SELF, assignee: "", url: "" }); await state.saveEditor();
    expect(api.publishEventPlanning).toHaveBeenLastCalledWith(ROOM, expect.objectContaining({ assignee: null, url: null }));
  });
});

describe("public calendar planning association", () => {
  test("creates one private workspace and preserves the calendar master on linking", async () => {
    const api = { ...client(), updateCommunityEvent: mock(async () => ({})) };
    const event = { id: "public-event", uid: "public-event", summary: "London", organizer: `xmpp:${SELF}`, dtstart: { kind: "date-time", ms: Date.parse("2027-02-12T09:00:00Z") }, rrule: { freq: "DAILY", count: 2 }, exdates: [{ kind: "date", date: "2027-02-13" }] };
    const state = await setupVueComponent("../src/components/community/EventsPane.vue", { client: api, events: [event], isLoading: false, isPosting: false, error: null, canPost: true, selfJid: SELF, communityJid: "community.example.com", serverBaseUrl: "", sessionId: null, findMaster: () => event }, import.meta.url) as unknown as { createPlanning: (event: unknown) => Promise<void> };
    await state.createPlanning(event);
    expect(api.createEventRoom).toHaveBeenCalledWith("London", true);
    expect(api.updateCommunityEvent).toHaveBeenCalledWith("community.example.com", "public-event", expect.objectContaining({ workspaceRoom: ROOM, rrule: event.rrule, exdates: event.exdates, dtstart: event.dtstart }));
    expect(api.publishEventPlanning).toHaveBeenCalledWith(ROOM, expect.objectContaining({ id: "event", title: "London", starts_at: "2027-02-12T09:00:00.000Z" }));
  });
  test("translates typed workspace relations across the WASM boundary", async () => {
    const { eventFromWasm } = await import("../src/lib/xmpp/event-types");
    const { PubsubManager } = await import("../src/lib/xmpp/client-pubsub");
    const publish = mock(async () => ({ id: "e", uid: "e", summary: "London", workspace_room: ROOM }));
    const manager = new PubsubManager({ sessionJid: () => SELF, requireConnectedXmpp: async () => ({ xcal_publish: publish }) });
    const result = await manager.publishCommunityEvent("community.example.com", { summary: "London", workspaceRoom: ROOM });
    expect(publish).toHaveBeenCalledWith("community.example.com", expect.objectContaining({ workspace_room: ROOM }));
    expect(result.workspaceRoom).toBe(ROOM);
    expect(eventFromWasm({ id: "e", uid: "e", summary: "London", workspace_room: ROOM }).workspaceRoom).toBe(ROOM);
  });
});
