import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { nextTick, ref } from "vue";
import { useChatTabUnreadIndicator } from "../src/shell/tab-unread-indicator";

const originalWindow = globalThis.window;
const originalDocument = globalThis.document;
const originalNavigator = globalThis.navigator;
const originalImage = globalThis.Image;
let autoLoadImages = true;
let pendingImages: FakeImage[] = [];

class FakeLinkElement {
  rel: string;
  type: string;
  href: string;
  private attributes: Map<string, string>;

  constructor(attributes: Record<string, string> = {
    rel: "icon",
    type: "image/png",
    sizes: "32x32",
    href: "/favicon-32x32.png",
  }) {
    this.attributes = new Map(Object.entries(attributes));
    this.rel = attributes.rel ?? "icon";
    this.type = attributes.type ?? "";
    this.href = attributes.href ?? "";
  }

  getAttribute(name: string) {
    return this.attributes.get(name) ?? null;
  }

  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
    if (name === "href") this.href = value;
    if (name === "type") this.type = value;
    if (name === "rel") this.rel = value;
  }

  removeAttribute(name: string) {
    this.attributes.delete(name);
    if (name === "type") this.type = "";
  }
}

class FakeImage {
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  private value = "";

  get src() {
    return this.value;
  }

  set src(value: string) {
    this.value = value;
    if (!autoLoadImages) {
      pendingImages.push(this);
      return;
    }
    queueMicrotask(() => this.onload?.());
  }
}

let links: FakeLinkElement[];
let serviceWorker: EventTarget;
let setAppBadge: ReturnType<typeof mock>;
let clearAppBadge: ReturnType<typeof mock>;
let querySelectorAll: ReturnType<typeof mock>;
let canvasContext: {
  drawImage: ReturnType<typeof mock>;
  beginPath: ReturnType<typeof mock>;
  arc: ReturnType<typeof mock>;
  fill: ReturnType<typeof mock>;
  stroke: ReturnType<typeof mock>;
  fillStyle: string;
  lineWidth: number;
  strokeStyle: string;
};

async function flushPromises() {
  await nextTick();
  await Promise.resolve();
  await Promise.resolve();
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

beforeEach(() => {
  autoLoadImages = true;
  pendingImages = [];
  links = [
    new FakeLinkElement({ rel: "icon", type: "image/svg+xml", href: "/waddle-logo.svg" }),
    new FakeLinkElement({ rel: "icon", type: "image/png", sizes: "32x32", href: "/favicon-32x32.png" }),
    new FakeLinkElement({ rel: "icon", type: "image/png", sizes: "16x16", href: "/favicon-16x16.png" }),
    new FakeLinkElement({ rel: "shortcut icon", href: "/favicon.ico" }),
  ];
  serviceWorker = new EventTarget();
  setAppBadge = mock(async (_count?: number) => {});
  clearAppBadge = mock(async () => {});
  canvasContext = {
    drawImage: mock(() => {}),
    beginPath: mock(() => {}),
    arc: mock(() => {}),
    fill: mock(() => {}),
    stroke: mock(() => {}),
    fillStyle: "",
    lineWidth: 0,
    strokeStyle: "",
  };

  querySelectorAll = mock((_selector: string) => links);

  const documentMock = {
    title: "Waddle",
    head: {
      appendChild: mock((_node: unknown) => {}),
    },
    querySelectorAll,
    createElement: mock((tagName: string) => {
      if (tagName === "canvas") {
        return {
          width: 0,
          height: 0,
          getContext: mock(() => canvasContext),
          toDataURL: mock(() => "data:image/png;base64,unread-dot"),
        };
      }
      return new FakeLinkElement();
    }),
  };

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: new EventTarget(),
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    writable: true,
    value: documentMock,
  });
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    writable: true,
    value: {
      serviceWorker,
      setAppBadge,
      clearAppBadge,
    },
  });
  Object.defineProperty(globalThis, "Image", {
    configurable: true,
    writable: true,
    value: FakeImage,
  });
});

afterEach(() => {
  if (originalWindow === undefined) {
    Reflect.deleteProperty(globalThis, "window");
  } else {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      writable: true,
      value: originalWindow,
    });
  }

  if (originalDocument === undefined) {
    Reflect.deleteProperty(globalThis, "document");
  } else {
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      writable: true,
      value: originalDocument,
    });
  }

  if (originalNavigator === undefined) {
    Reflect.deleteProperty(globalThis, "navigator");
  } else {
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      writable: true,
      value: originalNavigator,
    });
  }

  if (originalImage === undefined) {
    Reflect.deleteProperty(globalThis, "Image");
  } else {
    Object.defineProperty(globalThis, "Image", {
      configurable: true,
      writable: true,
      value: originalImage,
    });
  }
});

describe("useChatTabUnreadIndicator", () => {
  test("updates title, favicon, and app badge for unread counts", async () => {
    const count = ref(3);
    const indicator = useChatTabUnreadIndicator(count);

    await flushPromises();

    expect(document.title).toBe("* Waddle");
    expect(setAppBadge).toHaveBeenCalledWith(3);
    expect(querySelectorAll).toHaveBeenCalledWith('link[rel~="icon"]');
    expect(links.map((item) => item.href)).toEqual([
      "data:image/png;base64,unread-dot",
      "data:image/png;base64,unread-dot",
      "data:image/png;base64,unread-dot",
      "data:image/png;base64,unread-dot",
    ]);
    expect(links.map((item) => item.getAttribute("type"))).toEqual([
      "image/png",
      "image/png",
      "image/png",
      "image/png",
    ]);
    expect(canvasContext.arc).toHaveBeenCalledWith(28, 4, 5, 0, Math.PI * 2);
    expect(canvasContext.fillStyle).toBe("#dc2626");
    expect(canvasContext.lineWidth).toBe(2);
    expect(canvasContext.strokeStyle).toBe("#ffffff");
    expect(canvasContext.stroke).toHaveBeenCalled();

    count.value = 0;
    await flushPromises();

    expect(document.title).toBe("Waddle");
    expect(clearAppBadge).toHaveBeenCalled();
    expect(links.map((item) => item.href)).toEqual([
      "/waddle-logo.svg",
      "/favicon-32x32.png",
      "/favicon-16x16.png",
      "/favicon.ico",
    ]);
    expect(links.map((item) => item.getAttribute("type"))).toEqual([
      "image/svg+xml",
      "image/png",
      "image/png",
      null,
    ]);

    indicator.stop();
  });

  test("uses service worker unread messages as a tab sync nudge", async () => {
    const count = ref(0);
    const hydration = deferred<boolean>();
    const onServiceWorkerUnreadCount = mock((_count: number) => hydration.promise);
    const indicator = useChatTabUnreadIndicator(count, { onServiceWorkerUnreadCount });
    await flushPromises();

    const event = Object.assign(new Event("message"), {
      data: { type: "waddle:unread-count", unreadCount: 5 },
    });
    serviceWorker.dispatchEvent(event);
    await flushPromises();

    expect(onServiceWorkerUnreadCount).toHaveBeenCalledWith(5);
    expect(document.title).toBe("* Waddle");
    expect(setAppBadge).toHaveBeenCalledWith(5);

    count.value = 5;
    hydration.resolve(true);
    await flushPromises();

    expect(document.title).toBe("* Waddle");
    expect(setAppBadge).toHaveBeenCalledWith(5);

    indicator.stop();
  });

  test("reconciles deferred local changes when inbox refresh does not complete", async () => {
    const count = ref(0);
    const hydration = deferred<boolean>();
    const indicator = useChatTabUnreadIndicator(count, {
      onServiceWorkerUnreadCount: mock((_count: number) => hydration.promise),
    });
    await flushPromises();

    serviceWorker.dispatchEvent(Object.assign(new Event("message"), {
      data: { type: "waddle:unread-count", unreadCount: 7 },
    }));
    await flushPromises();

    count.value = 2;
    await flushPromises();

    hydration.resolve(false);
    await flushPromises();

    expect(document.title).toBe("* Waddle");
    expect(setAppBadge).toHaveBeenCalledWith(2);

    indicator.stop();
  });

  test("ignores in-flight favicon and service worker work after stop", async () => {
    autoLoadImages = false;
    const count = ref(3);
    const hydration = deferred<boolean>();
    const indicator = useChatTabUnreadIndicator(count, {
      onServiceWorkerUnreadCount: mock((_count: number) => hydration.promise),
    });
    await nextTick();

    expect(document.title).toBe("* Waddle");

    serviceWorker.dispatchEvent(Object.assign(new Event("message"), {
      data: { type: "waddle:unread-count", unreadCount: 8 },
    }));
    await flushPromises();
    expect(document.title).toBe("* Waddle");

    indicator.stop();
    count.value = 9;
    hydration.resolve(true);
    for (const image of pendingImages) {
      image.onload?.();
    }
    await flushPromises();

    expect(document.title).toBe("Waddle");
    expect(links.map((item) => item.href)).toEqual([
      "/waddle-logo.svg",
      "/favicon-32x32.png",
      "/favicon-16x16.png",
      "/favicon.ico",
    ]);
    expect(setAppBadge).not.toHaveBeenCalledWith(9);
  });

  test("ignores service worker unread messages when the app should not accept them", async () => {
    const count = ref(0);
    const onServiceWorkerUnreadCount = mock((_count: number) => true);
    const indicator = useChatTabUnreadIndicator(count, {
      shouldAcceptServiceWorkerUnreadCount: () => false,
      onServiceWorkerUnreadCount,
    });
    await flushPromises();

    serviceWorker.dispatchEvent(Object.assign(new Event("message"), {
      data: { type: "waddle:unread-count", unreadCount: 9 },
    }));
    await flushPromises();

    expect(onServiceWorkerUnreadCount).not.toHaveBeenCalled();
    expect(document.title).toBe("Waddle");
    expect(clearAppBadge).toHaveBeenCalled();

    indicator.stop();
  });
});
