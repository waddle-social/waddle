import { describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import type { CallAnchorCardState } from "../src/lib/call-thread-anchor";

const liveState: CallAnchorCardState = {
  status: "live",
  media: { audio: true, video: true },
  participantCount: 2,
  participantLabels: ["alice", "bob"],
  messageCount: 7,
  threadId: "call-thread-uuid",
  title: "Live video call",
  actionLabel: "Join",
  actionDisabled: false,
  ariaLabel: "Join live video call, 2 people: alice, bob",
};

describe("CallAnchorCard", () => {
  test("renders the live call anchor with media, participants, join, and call-chat count", async () => {
    const html = await renderVueComponent("../src/components/calls/CallAnchorCard.vue", {
      state: liveState,
    }, import.meta.url);

    expect(html).toContain("Live video call");
    expect(html).toContain("call-anchor-card__pulse");
    expect(html).toContain("alice");
    expect(html).toContain("bob");
    expect(html).toContain("Join");
    expect(html).toContain("7 messages in call chat");
    expect(html).toContain('aria-label="Join live video call, 2 people: alice, bob"');
  });

  test("the action button's accessible name is the visible action label, not the full description", async () => {
    const html = await renderVueComponent("../src/components/calls/CallAnchorCard.vue", {
      state: liveState,
    }, import.meta.url);

    // The descriptive aria-label belongs only to the <section>; the action
    // button must not repeat it (screen readers would announce it twice).
    const ariaLabelCount = html.split('aria-label="Join live video call, 2 people: alice, bob"').length - 1;
    expect(ariaLabelCount).toBe(1);
    // The button carries no aria-label, so its accessible name is the visible
    // "Join" text inside its <span>.
    const button = html.match(/<button[^>]*bg-primary[^>]*>([\s\S]*?)<\/button>/);
    expect(button).not.toBeNull();
    expect(button?.[0]).not.toContain("aria-label");
    expect(button?.[1]).toContain("<span>Join</span>");
  });

  test("renders ended call anchors muted without a join action", async () => {
    const html = await renderVueComponent("../src/components/calls/CallAnchorCard.vue", {
      state: {
        ...liveState,
        status: "ended",
        participantCount: 0,
        participantLabels: [],
        title: "Call ended",
        actionLabel: null,
        actionDisabled: false,
        ariaLabel: "Call ended · 5m",
      } satisfies CallAnchorCardState,
    }, import.meta.url);

    expect(html).toContain("Call ended");
    expect(html).toContain("call-anchor-card--ended");
    expect(html).not.toContain(">Join<");
    expect(html).toContain('aria-label="Call ended · 5m"');
  });
});
