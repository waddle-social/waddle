import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import type { WasmThreadEntry } from "../src/lib/xmpp/wasm-types";
import { $callState } from "../src/lib/calls/call-store";
import { $mucCallMedia, $mucCallParticipants, clearMucCallParticipants } from "../src/lib/calls/muc-call-presence";

const baseEntry: WasmThreadEntry = {
  channel: "general@conference.example.com",
  thread_id: "t1",
  last_stanza_id: "s",
  last_activity: "2026-06-07T14:30:00Z",
  unread: 0,
  reply_count: 7,
  has_unread: false,
};

function renderRow(entry: WasmThreadEntry) {
  return renderVueComponent("../src/components/chat/ThreadsListRow.vue", { entry }, import.meta.url);
}

describe("ThreadsListRow call-thread anchors", () => {
  afterEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    $mucCallMedia.set({});
  });

  test("renders the live call anchor card for a live MUC call-thread row", async () => {
    $mucCallParticipants.set({ "general@conference.example.com": ["alice", "bob"] });
    $mucCallMedia.setKey("general@conference.example.com", { audio: true, video: true });

    const html = await renderRow({
      ...baseEntry,
      callThread: { kind: "muc", media: ["audio", "video"] },
    });

    expect(html).toContain("call-anchor-card__pulse");
    expect(html).toContain("Join");
    expect(html).toContain("7 messages in call chat");
  });

  test("renders the ended call anchor card without a join action", async () => {
    const html = await renderRow({
      ...baseEntry,
      callThread: { kind: "muc", media: ["audio"] },
      callThreadEnded: { ended: "2026-06-07T14:35:00Z", duration: "PT5M" },
    });

    expect(html).toContain("call-anchor-card--ended");
    expect(html).toContain("Call ended · 5m");
    expect(html).not.toContain(">Join<");
  });

  test("renders the plain title row for a non-call thread", async () => {
    const html = await renderRow({
      ...baseEntry,
      thread_title: "Roadmap planning",
    });

    expect(html).not.toContain("call-anchor-card");
    expect(html).toContain("Roadmap planning");
  });
});
