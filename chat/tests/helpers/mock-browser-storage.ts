import { afterEach, beforeEach } from "bun:test";

type StorageMock = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
  clear(): void;
};

export function createStorageMock(): StorageMock {
  const values = new Map<string, string>();
  return {
    getItem(key: string) {
      return values.has(key) ? values.get(key)! : null;
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
    removeItem(key: string) {
      values.delete(key);
    },
    clear() {
      values.clear();
    },
  };
}

/**
 * Registers the shared `beforeEach`/`afterEach` pair that swaps
 * `globalThis.localStorage` and `globalThis.window` for per-test mocks and
 * restores the originals afterwards. Call once at module scope (or inside a
 * `describe`) of a test file.
 *
 * - `windowListeners`: add no-op `addEventListener`/`removeEventListener`
 *   stubs to the window mock.
 * - `beforeEachExtra`: runs at the end of the shared `beforeEach`.
 * - `afterEachExtra`: runs after `localStorage.clear()` in the shared
 *   `afterEach`, before the globals are restored.
 */
export function installMockBrowserGlobals(options?: {
  windowListeners?: boolean;
  beforeEachExtra?: () => void;
  afterEachExtra?: () => void;
}): void {
  const originalWindow = globalThis.window;
  const originalLocalStorage = globalThis.localStorage;

  beforeEach(() => {
    const storage = createStorageMock();
    (globalThis as typeof globalThis & { localStorage: typeof storage }).localStorage = storage;
    (globalThis as typeof globalThis & { window: Window & { localStorage: typeof storage } }).window =
      {
        ...(originalWindow ?? {}),
        ...(options?.windowListeners
          ? {
              addEventListener: () => undefined,
              removeEventListener: () => undefined,
            }
          : {}),
        localStorage: storage,
      } as Window & { localStorage: typeof storage };
    localStorage.clear();
    options?.beforeEachExtra?.();
  });

  afterEach(() => {
    localStorage.clear();
    options?.afterEachExtra?.();
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      (globalThis as typeof globalThis & { localStorage: Storage }).localStorage =
        originalLocalStorage;
    }
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      (globalThis as typeof globalThis & { window: Window & typeof globalThis }).window =
        originalWindow;
    }
  });
}
