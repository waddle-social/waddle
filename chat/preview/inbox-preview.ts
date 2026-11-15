import { connectionStore } from "@/lib/connection-store";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { BrowserXmppClient, LiveRoomMessage } from "@/lib/xmpp-client";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import type { ChatAppController } from "@/shell/chat-app-controller";

/**
 * Preview fixtures for the Inbox and Home screens (`?screen=inbox-…`):
 *   inbox-unread | inbox-caught-up
 *   inbox-threads | inbox-threads-loading | inbox-threads-error
 *     (add &status=unread or &q=… for the filtered / empty states)
 *   inbox-home-empty   Home with no rooms, people or activity
 *   inbox-thread | inbox-thread-empty | inbox-thread-loading | inbox-thread-nested
 *     the thread panel over #releases
 */
const room = (name: string) => `${name}@muc.waddle.test`;
const minutesAgo = (m: number) => new Date(Date.now() - m * 60_000).toISOString();

const threadRows: WasmThreadEntry[] = [
  { channel: room("releases"), thread_id: "t1", last_stanza_id: "a", last_activity: minutesAgo(2), unread: 2, reply_count: 3, has_unread: true, root_author: "jo", thread_title: "@ada can you check MUC history on Android before we tag?" },
  { channel: room("design"), thread_id: "t2", last_stanza_id: "b", last_activity: minutesAgo(25), unread: 5, reply_count: 11, has_unread: true, root_author: "kim", thread_title: "Daylight palette: is fg-soft too faint for timestamps?" },
  { channel: room("releases"), thread_id: "t3", last_stanza_id: "c", last_activity: minutesAgo(60), unread: 0, reply_count: 6, has_unread: false, root_author: "mika", thread_title: "Release go/no-go" },
  { channel: room("general"), thread_id: "t4", last_stanza_id: "d", last_activity: minutesAgo(180), unread: 0, reply_count: 8, has_unread: false, root_author: "priya", thread_title: "TestFlight build 214 crashes on launch with an empty roster" },
  { channel: room("ideas"), thread_id: "t5", last_stanza_id: "e", last_activity: minutesAgo(1500), unread: 0, reply_count: 19, has_unread: false, root_author: "theo", thread_title: "Should stories expire after 24h or 48h?" },
];

function live(roomJid: string, id: string, nick: string, body: string, minutes: number, threadId?: string): LiveRoomMessage {
  return { id, roomJid, nick, body, createdAt: minutesAgo(minutes), createdAtSource: "archive", type: "message", ...(threadId ? { threadId } : {}) } as LiveRoomMessage;
}

function fakeClient(screen: string): BrowserXmppClient {
  const never = new Promise<never>(() => {});
  const methods: Record<string, (...args: never[]) => unknown> = {
    fetchThreads: async (opts?: { status?: string; search?: string }) => {
      if (screen === "inbox-threads-loading") return never;
      if (screen === "inbox-threads-error") throw new Error("The server took too long to answer.");
      let entries = threadRows;
      if (opts?.status === "unread") entries = entries.filter((e) => e.has_unread);
      const q = opts?.search?.toLowerCase();
      if (q) entries = entries.filter((e) => e.thread_title?.toLowerCase().includes(q));
      return { total: entries.length, unread_threads: entries.filter((e) => e.has_unread).length, entries };
    },
    queryMamPage: async (_space: unknown, channelId: unknown) => ({
      complete: true,
      messages: channelId === "design"
        ? [live(room("design"), "d1", "sam", "New identity drafts are up. The waddling typing dots are my favourite part.", 40),
           live(room("design"), "d2", "kim", "Daylight looks calm. I'd bump the timestamp contrast a touch.", 30),
           live(room("design"), "d3", "sam", "Agreed, trying fg-muted there now.", 20)]
        : [live(room("releases"), "r1", "jo", "@ada can you check MUC history on Android before we tag?", 12),
           live(room("releases"), "r2", "mika", "Server tests are green on main. Tagging the RC after the huddle.", 8)],
    }),
    queryMamThreadPage: async () => ({
      complete: true,
      messages: [live(room("releases"), "tr1", "ana", "Fixed locally, opening a PR. Safe to tag after it lands.", 3, "t1")],
    }),
  };
  const identity: Record<string, string> = { bareJid: "ada@waddle.test", fullJid: "ada@waddle.test/preview", xmppResource: "preview" };
  return new Proxy(methods, {
    get: (target, prop) => {
      if (typeof prop === "symbol" || prop === "then" || prop === "catchup" || prop === "agent") return undefined;
      if (prop.startsWith("list")) return () => [];
      return identity[prop] ?? target[prop] ?? (() => never);
    },
  }) as unknown as BrowserXmppClient;
}

function threadMessages(screen: string): TimelineMessage[] {
  const msg = (id: string, author: string, body: string, minutes: number, extra: Partial<TimelineMessage> = {}): TimelineMessage =>
    ({ id, author, body, createdAt: minutesAgo(minutes), isSelf: author === "ada", ...extra });
  const timeline = [
    msg("m0", "mika", "Server tests are green on main. Tagging the RC after the huddle.", 30),
    msg("t1", "jo", "@ada can you check MUC history on Android before we tag?", 14),
    msg("m2", "priya", "I'll take the Apple build. Anyone seen TestFlight 214 crash with an empty roster?", 12),
  ];
  if (screen === "inbox-thread-empty") return timeline;
  return [
    ...timeline,
    msg("c1", "ana", "On it. Pixel 8 shows the last 50 messages, then stops paging.", 10, { threadId: "t1" }),
    msg("c1a", "mika", "Which page size does it ask for?", 9, { threadId: "c1", parentThreadId: "t1" }),
    msg("c1b", "ana", "50, and it never sends the next query.", 8, { threadId: "c1", parentThreadId: "t1" }),
    msg("t1b", "mika", "That's the RSM page size. The server sends <fin complete='false'>, so the client should ask for the next page.", 7, { threadId: "t1" }),
    msg("t1c", "ana", "Fixed locally, opening a PR. Safe to tag after it lands.", 3, { threadId: "t1" }),
  ];
}

export function seedInbox(c: ChatAppController, screen: string): void {
  if (!screen.startsWith("inbox")) return;
  if (screen.startsWith("inbox-thread") && !screen.startsWith("inbox-threads")) {
    void c.selectChannel("releases");
    setTimeout(() => {
      c.messaging.messages.value = threadMessages(screen);
      if (screen === "inbox-thread-loading") c.openThread("missing");
      else c.openThread("t1");
      if (screen === "inbox-thread-nested") c.pushThread("c1");
    }, 300);
    return;
  }
  if (screen === "inbox-home-empty") {
    c.waddles.channels.value = [];
    c.waddles.groupDms.value = [];
    c.dmConversations.conversations.value = [];
    c.channelUnread.inboxState.value = { channels: new Map(), threads: new Map() };
    c.messaging.mentionedChannelCounts.value = {};
    return;
  }
  const page = screen.startsWith("inbox-threads") ? "threads" : "unread";
  if (screen === "inbox-caught-up") {
    c.channelUnread.inboxState.value = { channels: new Map(), threads: new Map() };
  } else if (page === "unread") {
    const now = Date.now();
    const muc = (partner: string, unread: number, lastUpdated: number, thread?: string, threadTitle?: string) =>
      ({ partner, kind: "muc" as const, lastStanzaId: `s${lastUpdated}`, lastUpdated, unread, ...(thread ? { thread, threadTitle } : {}) });
    c.channelUnread.inboxState.value = {
      channels: new Map([[room("releases"), muc(room("releases"), 2, now)], [room("design"), muc(room("design"), 3, now - 1)]]),
      threads: new Map([[`${room("releases")}::t1`, muc(room("releases"), 1, now, "t1", "@ada can you check MUC history on Android before we tag?")]]),
    };
  }
  // A new client resets the room directory; put the fixture rooms back.
  const { channels, waddles } = { channels: c.waddles.channels.value, waddles: c.waddles.waddles.value };
  connectionStore.client = fakeClient(screen);
  c.ui.activePage.value = page;
  // Route sync settles the /_preview URL onto home after seeding; land after it.
  setTimeout(() => {
    c.ui.activePage.value = page;
    c.waddles.waddles.value = waddles;
    c.waddles.channels.value = channels;
  }, 0);
}
