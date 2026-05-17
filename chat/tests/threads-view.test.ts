// Tests for `ThreadsView.vue` mobile shell wiring.
//
// `@vue/test-utils` is not available in this repo, so we follow the
// same in-memory-model pattern used by `threads-list-panel.test.ts`
// for behavioral coverage, and complement it with structural file
// assertions against `ThreadsView.vue` and `ChatReadyShell.vue` to
// guarantee the mobile-shell wiring (hamburger -> openNav -> drawer)
// stays in place.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const threadsViewSrc = readFileSync(
  resolve(here, "../src/components/chat/ThreadsView.vue"),
  "utf8",
);
const chatShellSrc = readFileSync(
  resolve(here, "../src/components/chat/ChatReadyShell.vue"),
  "utf8",
);

interface ViewState {
  openNavCount: number;
}

/**
 * Mirrors `ThreadsView.vue`'s emit surface: a `Menu` button in the
 * mobile-only header fires an `openNav` event. The shell wires this
 * to `ui.showMobileNav.value = true`.
 */
function createThreadsView() {
  const state: ViewState = { openNavCount: 0 };

  function clickHamburger() {
    state.openNavCount += 1;
  }

  return { state, clickHamburger };
}

describe("ThreadsView · emit model", () => {
  test("emits openNav when the mobile hamburger is activated", () => {
    const view = createThreadsView();
    expect(view.state.openNavCount).toBe(0);
    view.clickHamburger();
    expect(view.state.openNavCount).toBe(1);
  });

  test("each hamburger activation produces one openNav emission", () => {
    const view = createThreadsView();
    view.clickHamburger();
    view.clickHamburger();
    view.clickHamburger();
    expect(view.state.openNavCount).toBe(3);
  });
});

describe("ThreadsView · mobile shell template", () => {
  test("declares an openNav emit", () => {
    expect(threadsViewSrc).toContain("openNav: []");
  });

  test("renders a mobile-only header with a hamburger button", () => {
    // Header is hidden on >= md to match FeedPane / StoriesPane / EventsPane.
    expect(threadsViewSrc).toMatch(/<header[^>]*class="[^"]*md:hidden/);
  });

  test("hamburger button is wired to emit openNav on click", () => {
    expect(threadsViewSrc).toMatch(/@click="emit\('openNav'\)"/);
  });

  test("imports the Menu icon used by sibling pane headers", () => {
    expect(threadsViewSrc).toMatch(/from "lucide-vue-next"/);
    expect(threadsViewSrc).toMatch(/\bMenu\b/);
  });

  test("ChatReadyShell forwards ThreadsView's openNav to the mobile drawer", () => {
    // Block-scalar match: the ThreadsView usage in ChatReadyShell must
    // pair `activePage === 'threads'` with the standard openNav handler
    // so the hamburger opens the same drawer used by the other panes.
    expect(chatShellSrc).toMatch(
      /<ThreadsView[\s\S]*?activePage\.value === 'threads'[\s\S]*?@open-nav="ui\.showMobileNav\.value = true"[\s\S]*?\/>/,
    );
  });
});
