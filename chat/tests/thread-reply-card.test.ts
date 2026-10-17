import { describe, expect, test } from "bun:test";
import {
  latestThreadReply,
  threadReplyCardLabel,
  threadReplyCountLabel,
  threadReplySnippet,
} from "../src/components/chat/thread-reply-card";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { MessageThreadEntry } from "../src/channels/threads";
import { renderVueComponentSource } from "./helpers/render-vue-sfc";

function message(partial: Partial<TimelineMessage> & { id: string }): TimelineMessage {
  return {
    author: "guzzle",
    body: "",
    createdAt: "2026-10-17T10:00:00Z",
    createdAtSource: "archive",
    isSelf: false,
    ...partial,
  } as TimelineMessage;
}

function entry(children: TimelineMessage[]): MessageThreadEntry {
  return {
    threadId: "root",
    root: message({ id: "root", author: "rawkode", body: "https://rawkode.academy/watch/x" }),
    directChildren: children,
    allDescendants: children,
    count: children.length,
    lastTs: children.at(-1)?.createdAt ?? "",
  };
}

describe("threadReplySnippet", () => {
  test("collapses whitespace into one plain line", () => {
    expect(threadReplySnippet(message({ id: "a", body: "  Love this,\n\n  watching\tnow  " }))).toBe(
      "Love this, watching now",
    );
  });

  test("retracted replies read as deleted", () => {
    expect(threadReplySnippet(message({ id: "a", body: "secret", isRetracted: true }))).toBe("Message deleted");
  });

  test("attachment-only replies name the attachment kind", () => {
    const photo = message({
      id: "p",
      body: "https://files.example/cat.png",
      sharedFiles: [{ url: "https://files.example/cat.png", mediaType: "image/png", disposition: "inline" }],
    });
    expect(threadReplySnippet(photo)).toBe("Photo");
    expect(threadReplySnippet(message({ id: "u", body: "https://media.example/cat.gif" }))).toBe("Photo");
    const file = message({
      id: "f",
      body: "",
      sharedFiles: [{ url: "https://files.example/notes.pdf", mediaType: "application/pdf", disposition: "attachment" }],
    });
    expect(threadReplySnippet(file)).toBe("Attachment");
  });

  test("text beside an attachment wins over the attachment label", () => {
    const captioned = message({
      id: "c",
      body: "look at this",
      sharedFiles: [{ url: "https://files.example/cat.png", mediaType: "image/png", disposition: "inline" }],
    });
    expect(threadReplySnippet(captioned)).toBe("look at this");
  });

  test("/me bodies read as actions and long bodies are capped", () => {
    expect(threadReplySnippet(message({ id: "m", author: "guzzle", body: "/me waves" }))).toBe("* guzzle waves");
    const long = threadReplySnippet(message({ id: "l", body: "x".repeat(400) }));
    expect(long.length).toBe(140);
    expect(long.endsWith("…")).toBe(true);
  });

  test("a bodyless, fileless reply has no snippet", () => {
    expect(threadReplySnippet(message({ id: "e", body: "   " }))).toBe("");
  });
});

describe("latestThreadReply", () => {
  test("takes the newest loaded direct reply", () => {
    const latest = latestThreadReply(entry([
      message({ id: "r1", author: "alice", body: "first", createdAt: "2026-10-17T10:01:00Z" }),
      message({ id: "r2", author: "guzzle", body: "Love this, watching now", createdAt: "2026-10-17T10:02:00Z" }),
    ]));
    expect(latest).toEqual({
      author: "guzzle",
      snippet: "Love this, watching now",
      createdAt: "2026-10-17T10:02:00Z",
    });
  });

  test("is null when the count is known but no reply is loaded", () => {
    expect(latestThreadReply(entry([]))).toBeNull();
    expect(latestThreadReply(undefined)).toBeNull();
    expect(latestThreadReply(null)).toBeNull();
  });
});

describe("threadReplyCardLabel", () => {
  test("names the count, latest reply and time, then the action", () => {
    expect(threadReplyCountLabel(1)).toBe("1 reply");
    expect(threadReplyCountLabel(3)).toBe("3 replies");
    expect(threadReplyCardLabel({
      count: 3,
      latest: { author: "guzzle", snippet: "Love this, watching now", createdAt: "2026-10-17T10:02:00Z" },
      recency: "2 hours ago",
    })).toBe("3 replies, latest from guzzle: Love this, watching now, 2 hours ago. Open thread");
  });

  test("omits the parts it does not know", () => {
    expect(threadReplyCardLabel({ count: 1, latest: null, recency: "" })).toBe("1 reply. Open thread");
    expect(threadReplyCardLabel({
      count: 2,
      latest: { author: "alice", snippet: "", createdAt: "2026-10-17T10:02:00Z" },
      recency: "just now",
    })).toBe("2 replies, latest from alice, just now. Open thread");
  });
});

describe("MessageCard attached reply card", () => {
  const post = message({ id: "root", author: "rawkode", body: "https://rawkode.academy/watch/x" });
  const wrapper = `<script setup lang="ts">
import MessageCard from "@/components/chat/MessageCard.vue";
import type { TimelineMessage } from "@/lib/chat-ui";
defineProps<{ post: TimelineMessage; lastReplyAt: string }>();
const participants = [
  { key: "jid:guzzle@example.com", nick: "guzzle", jid: "guzzle@example.com", presence: "online" },
  { key: "jid:alice@example.com", nick: "alice", jid: "alice@example.com", presence: "offline" },
  { key: "jid:bob@example.com", nick: "bob", jid: "bob@example.com", presence: "offline" },
  { key: "jid:carol@example.com", nick: "carol", jid: "carol@example.com", presence: "offline" },
];
</script>
<template>
  <div>
    <section data-case="loaded">
      <MessageCard
        :message="post"
        :hats="[]"
        :thread-reply-count="3"
        :thread-participants="participants"
        :thread-last-reply-at="lastReplyAt"
        :thread-latest-reply="{ author: 'guzzle', snippet: 'Love this, watching now', createdAt: lastReplyAt }"
      />
    </section>
    <section data-case="count-only">
      <MessageCard :message="post" :hats="[]" :thread-reply-count="1" />
    </section>
    <section data-case="thread-pane">
      <MessageCard :message="post" :hats="[]" :thread-reply-count="2" hide-thread-chip />
    </section>
    <section data-case="no-replies">
      <MessageCard :message="post" :hats="[]" :thread-reply-count="0" />
    </section>
  </div>
</template>
`;

  // Template comments mention class names; SSR fragment markers split text.
  function section(rawHtml: string, name: string): string {
    const html = rawHtml.replace(/<!--[\s\S]*?-->/g, "");
    const start = html.indexOf(`data-case="${name}"`);
    expect(start).toBeGreaterThanOrEqual(0);
    const end = html.indexOf("</section>", start);
    return html.slice(start, end);
  }

  test("renders one labelled button with faces, count, recency and the latest reply", async () => {
    const lastReplyAt = new Date(Date.now() - 2 * 3_600_000).toISOString();
    const html = await renderVueComponentSource(wrapper, { post, lastReplyAt });

    const loaded = section(html, "loaded");
    const cards = loaded.match(/<button[^>]*class="chat-thread-card[^"]*"/g) ?? [];
    expect(cards).toHaveLength(1);
    expect(cards[0]).toContain("chat-thread-card--two-line");
    expect(loaded).toContain(
      'aria-label="3 replies, latest from guzzle: Love this, watching now, 2 hours ago. Open thread"',
    );
    expect(loaded.match(/class="chat-thread-card__face"/g) ?? []).toHaveLength(3);
    expect(loaded).toContain(">+1</span>");
    expect(loaded).toContain('<span class="chat-thread-card__count">3 replies</span>');
    expect(loaded).toContain('<span class="chat-thread-card__recency"> · 2 hours ago</span>');
    expect(loaded).toContain('<span class="chat-thread-card__latest-author">guzzle</span>: Love this, watching now');
    expect(loaded).toContain("chat-thread-rail chat-thread-rail--two-line");
    // Avatar and author name open the profile with the spec's label.
    expect(loaded.match(/aria-label="rawkode, open profile"/g) ?? []).toHaveLength(2);
  }, 30_000);

  test("omits the latest-reply line when no reply is loaded", async () => {
    const html = await renderVueComponentSource(wrapper, { post, lastReplyAt: "" });
    const countOnly = section(html, "count-only");
    expect(countOnly).toContain('aria-label="1 reply. Open thread"');
    expect(countOnly).not.toContain("chat-thread-card__latest");
    expect(countOnly).not.toContain("chat-thread-card--two-line");
  }, 30_000);

  test("no card in the thread pane or on posts without replies", async () => {
    const html = await renderVueComponentSource(wrapper, { post, lastReplyAt: "" });
    for (const name of ["thread-pane", "no-replies"]) {
      const body = section(html, name);
      expect(body).not.toContain("chat-thread-card");
      expect(body).not.toContain("chat-thread-rail");
    }
  }, 30_000);
});
