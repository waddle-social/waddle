import { describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import type { ThreadParticipant } from "../src/components/chat/thread-lobby-meta";

const baseProps = {
  breadcrumbLabels: ["root thread"],
  threadPreview: "",
  callAnchorState: null,
  rootAuthor: null,
  replyCount: 0,
  lastActivityLabel: "",
  participants: [] as ThreadParticipant[],
};

function render(props: Record<string, unknown> = {}) {
  return renderVueComponent(
    "../src/components/chat/ThreadPanelHeader.vue",
    { ...baseProps, ...props },
    import.meta.url,
  );
}

describe("ThreadPanelHeader", () => {
  test("single-thread depth shows the root preview, not the breadcrumb trail", async () => {
    const html = await render({ threadPreview: "root body preview" });
    expect(html).toContain("chat-thread-header__preview");
    expect(html).toContain("root body preview");
    expect(html).not.toContain("chat-thread-header__breadcrumb");
    expect(html).not.toContain("Go back");
  });

  test("falls back to a placeholder title without a preview", async () => {
    const html = await render();
    expect(html).toContain("chat-thread-header__preview--empty");
    expect(html).toContain(">Thread</p>");
  });

  test("nested threads render a breadcrumb trail with the active crumb marked", async () => {
    const html = await render({ breadcrumbLabels: ["root thread", "sub thread"] });
    expect(html).toContain("chat-thread-header__breadcrumb");
    expect(html).toContain(">root thread</button>");
    expect(html).toContain(">sub thread</button>");
    const activeIndex = html.indexOf("chat-thread-header__breadcrumb-crumb--active");
    expect(activeIndex).toBeGreaterThan(html.indexOf("root thread"));
    // Mobile back affordance only exists when there is somewhere to pop to.
    expect(html).toContain("Go back");
  });

  test("meta row pluralises replies and shows author and activity pulse", async () => {
    const single = await render({ rootAuthor: "alice", replyCount: 1, lastActivityLabel: "just now" });
    expect(single).toContain("Started by");
    expect(single).toContain("alice");
    expect(single).toContain("<strong>1</strong>");
    expect(single).toContain("reply");
    expect(single).toContain("chat-thread-header__pulse");
    expect(single).toContain("active just now");

    const plural = await render({ replyCount: 3 });
    expect(plural).toContain("<strong>3</strong>");
    expect(plural).toContain("replies");
    expect(plural).not.toContain("Started by");
    expect(plural).not.toContain("chat-thread-header__pulse");
  });

  test("participant stack caps at four avatars with an overflow chip", async () => {
    const participants: ThreadParticipant[] = ["a", "b", "c", "d", "e", "f"].map((nick) => ({
      nick,
      avatarUrl: null,
      presence: "offline",
    }));
    const html = await render({ participants });
    expect(html.match(/chat-thread-header__avatar-wrap/g)?.length).toBe(4);
    expect(html).toContain("chat-thread-header__overflow");
    expect(html).toContain("+2");

    const none = await render({ participants: [] });
    expect(none).not.toContain("chat-thread-header__participants");
  });
});
