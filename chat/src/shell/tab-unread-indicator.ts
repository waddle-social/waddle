import { getCurrentInstance, onUnmounted, watch, type Ref } from "vue";

const BASE_TITLE = "Waddle";
const UNREAD_TITLE = "* Waddle";
const FAVICON_URL = "/favicon-32x32.png";
const MESSAGE_TYPE = "waddle:unread-count";

interface TabUnreadIndicatorOptions {
  onServiceWorkerUnreadCount?: (unreadCount: number) => boolean | Promise<boolean>;
  shouldAcceptServiceWorkerUnreadCount?: () => boolean;
}

type BadgeNavigator = Navigator & {
  setAppBadge?: (contents?: number) => Promise<void>;
  clearAppBadge?: () => Promise<void>;
};

type FaviconLink = HTMLLinkElement & {
  href: string;
};

interface FaviconSnapshot {
  link: FaviconLink;
  href: string;
  type: string | null;
  sizes: string | null;
}

export function useChatTabUnreadIndicator(
  unreadCount: Readonly<Ref<number>>,
  options: TabUnreadIndicatorOptions = {},
) {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return { stop: () => {} };
  }

  const faviconLinks = ensureFaviconLinks();
  let unreadFaviconHref: string | null = null;
  let faviconGeneration = 0;
  let serviceWorkerRefreshId = 0;
  let deferLocalUnreadUpdates = false;
  let deferredLocalUnreadChanged = false;
  let disposed = false;

  async function applyUnreadCount(value: number) {
    if (disposed) return;
    const count = normalizeUnreadCount(value);
    document.title = count > 0 ? UNREAD_TITLE : BASE_TITLE;
    void updateAppBadge(count);
    await updateFavicons(
      count,
      faviconLinks,
      () => ++faviconGeneration,
      () => faviconGeneration,
      () => disposed,
    );
  }

  async function updateFavicons(
    count: number,
    links: FaviconSnapshot[],
    nextGeneration: () => number,
    currentGeneration: () => number,
    isDisposed: () => boolean,
  ) {
    const generation = nextGeneration();
    if (isDisposed()) return;
    if (count <= 0) {
      restoreFaviconLinks(links);
      return;
    }

    try {
      unreadFaviconHref ??= await drawUnreadFavicon();
    } catch {
      unreadFaviconHref = FAVICON_URL;
    }
    if (isDisposed() || generation !== currentGeneration()) return;
    for (const snapshot of links) {
      snapshot.link.href = unreadFaviconHref;
      snapshot.link.setAttribute("href", unreadFaviconHref);
      snapshot.link.setAttribute("type", "image/png");
    }
  }

  const stopWatch = watch(
    unreadCount,
    (count) => {
      if (deferLocalUnreadUpdates) {
        deferredLocalUnreadChanged = true;
        return;
      }
      void applyUnreadCount(count);
    },
    { immediate: true },
  );

  const handleServiceWorkerMessage = (event: MessageEvent) => {
    if (disposed) return;
    const data = event.data as { type?: string; unreadCount?: unknown } | undefined;
    if (data?.type !== MESSAGE_TYPE) return;
    if (options.shouldAcceptServiceWorkerUnreadCount && !options.shouldAcceptServiceWorkerUnreadCount()) {
      void applyUnreadCount(unreadCount.value);
      return;
    }
    const serviceWorkerUnreadCount = normalizeUnreadCount(data.unreadCount);
    const refreshId = ++serviceWorkerRefreshId;
    deferLocalUnreadUpdates = true;
    deferredLocalUnreadChanged = false;
    void applyUnreadCount(serviceWorkerUnreadCount);
    void Promise.resolve(options.onServiceWorkerUnreadCount?.(serviceWorkerUnreadCount))
      .catch(() => false)
      .then((didRefresh) => {
        if (disposed || refreshId !== serviceWorkerRefreshId) return;
        const shouldApplyLocalUnread = didRefresh || deferredLocalUnreadChanged;
        deferLocalUnreadUpdates = false;
        deferredLocalUnreadChanged = false;
        if (shouldApplyLocalUnread) {
          void applyUnreadCount(unreadCount.value);
        }
      });
  };

  navigator.serviceWorker?.addEventListener("message", handleServiceWorkerMessage);

  function stop() {
    if (disposed) return;
    disposed = true;
    faviconGeneration += 1;
    serviceWorkerRefreshId += 1;
    deferLocalUnreadUpdates = false;
    deferredLocalUnreadChanged = false;
    stopWatch();
    navigator.serviceWorker?.removeEventListener("message", handleServiceWorkerMessage);
    document.title = BASE_TITLE;
    restoreFaviconLinks(faviconLinks);
    void updateAppBadge(0);
  }

  if (getCurrentInstance()) {
    onUnmounted(stop);
  }

  return { stop };
}

function normalizeUnreadCount(value: unknown): number {
  const parsed = typeof value === "number" ? value : Number(value ?? 0);
  if (!Number.isFinite(parsed) || parsed <= 0) return 0;
  return Math.floor(parsed);
}

function ensureFaviconLinks(): FaviconSnapshot[] {
  const existing = Array.from(document.querySelectorAll<HTMLLinkElement>('link[rel~="icon"]'));
  if (existing.length > 0) return existing.map((link) => snapshotFaviconLink(link as FaviconLink));

  const link = document.createElement("link") as FaviconLink;
  link.rel = "icon";
  link.type = "image/png";
  link.setAttribute("sizes", "32x32");
  link.href = FAVICON_URL;
  link.setAttribute("href", FAVICON_URL);
  document.head.appendChild(link);
  return [snapshotFaviconLink(link)];
}

function snapshotFaviconLink(link: FaviconLink): FaviconSnapshot {
  return {
    link,
    href: link.getAttribute("href") || link.href || FAVICON_URL,
    type: link.getAttribute("type"),
    sizes: link.getAttribute("sizes"),
  };
}

function restoreFaviconLinks(links: FaviconSnapshot[]) {
  for (const snapshot of links) {
    snapshot.link.href = snapshot.href;
    snapshot.link.setAttribute("href", snapshot.href);
    restoreOptionalAttribute(snapshot.link, "type", snapshot.type);
    restoreOptionalAttribute(snapshot.link, "sizes", snapshot.sizes);
  }
}

function restoreOptionalAttribute(link: FaviconLink, name: string, value: string | null) {
  if (value === null) {
    link.removeAttribute(name);
    return;
  }
  link.setAttribute(name, value);
}

async function updateAppBadge(count: number) {
  const badgeNavigator = navigator as BadgeNavigator;
  try {
    if (count > 0 && typeof badgeNavigator.setAppBadge === "function") {
      await badgeNavigator.setAppBadge(count);
      return;
    }
    if (count === 0 && typeof badgeNavigator.clearAppBadge === "function") {
      await badgeNavigator.clearAppBadge();
    }
  } catch {
    // best-effort browser chrome integration
  }
}

async function drawUnreadFavicon(): Promise<string> {
  const image = await loadImage(FAVICON_URL);
  const canvas = document.createElement("canvas");
  const size = 32;
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext("2d");
  if (!context) return FAVICON_URL;

  context.drawImage(image, 0, 0, size, size);
  context.fillStyle = "#dc2626";
  context.lineWidth = 2;
  context.strokeStyle = "#ffffff";
  context.beginPath();
  context.arc(28, 4, 5, 0, Math.PI * 2);
  context.fill();
  context.stroke();

  return canvas.toDataURL("image/png");
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error(`Failed to load favicon ${src}`));
    image.src = src;
  });
}
