export type ScrollDirectionMode = "chat" | "social";

export const SCROLL_DIRECTION_STORAGE_KEY = "waddle:scroll-direction";

type StorageReader = Pick<Storage, "getItem">;
type StorageWriter = Pick<Storage, "removeItem" | "setItem">;

export function isScrollDirectionMode(value: string | null): value is ScrollDirectionMode {
  return value === "chat" || value === "social";
}

export function readStoredScrollDirection(storage?: StorageReader | null): ScrollDirectionMode {
  const resolvedStorage = storage
    ?? (typeof localStorage === "undefined" ? null : localStorage);
  const value = resolvedStorage?.getItem(SCROLL_DIRECTION_STORAGE_KEY) ?? null;
  return isScrollDirectionMode(value) ? value : "chat";
}

export function writeStoredScrollDirection(
  value: ScrollDirectionMode,
  storage?: StorageWriter | null,
) {
  const resolvedStorage = storage
    ?? (typeof localStorage === "undefined" ? null : localStorage);
  if (!resolvedStorage) return;

  if (value === "chat") {
    resolvedStorage.removeItem(SCROLL_DIRECTION_STORAGE_KEY);
  } else {
    resolvedStorage.setItem(SCROLL_DIRECTION_STORAGE_KEY, value);
  }
}

export function isTopPinnedScrollDirection(value: ScrollDirectionMode): boolean {
  return value === "social";
}

export function orderTimelineForScrollDirection<T>(
  items: readonly T[],
  value: ScrollDirectionMode,
): T[] {
  return isTopPinnedScrollDirection(value) ? [...items].reverse() : [...items];
}

export function getNewMessagesDividerPlacement(
  value: ScrollDirectionMode,
): "before" | "after" {
  return isTopPinnedScrollDirection(value) ? "after" : "before";
}

export function getPinnedScrollTop(
  el: Pick<HTMLElement, "scrollHeight">,
  value: ScrollDirectionMode,
): number {
  return isTopPinnedScrollDirection(value) ? 0 : el.scrollHeight;
}
