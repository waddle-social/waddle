/**
 * A tiny IndexedDB-backed store for the user's *uploaded* background image. The
 * device-prefs payload only carries a `ref` token (so it stays small and
 * JSON-serialisable); the image bytes live here and survive reloads.
 *
 * Single slot: a new upload overwrites the previous one and mints a fresh `ref`.
 * `load` checks the ref so a stale pref (pointing at an overwritten upload)
 * resolves to `null` — the processor then fails open to the raw camera.
 *
 * Thin I/O boundary, verified manually (there is no IndexedDB in the test
 * runtime). The reconcile/selection logic that drives it is unit-tested.
 */

const DB_NAME = "waddle-call-backgrounds";
const STORE = "custom-image";
const SLOT = "current";

type CustomImageRecord = { ref: string; blob: Blob };

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function withStore<T>(
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db = await openDb();
  try {
    return await new Promise<T>((resolve, reject) => {
      const tx = db.transaction(STORE, mode);
      const request = run(tx.objectStore(STORE));
      // Resolve on commit (oncomplete), not request.onsuccess, so a write is
      // durable before we report success. Reject AND release the connection on
      // every terminal path — including abort/error (e.g. a QuotaExceededError
      // saving a large upload), which would otherwise leak the open connection.
      tx.oncomplete = () => resolve(request.result);
      tx.onabort = () => reject(tx.error);
      tx.onerror = () => reject(tx.error);
      request.onerror = () => reject(request.error);
    });
  } finally {
    db.close();
  }
}

/** Persist an uploaded image, replacing any previous one, and return its ref. */
export async function saveCustomBackground(blob: Blob): Promise<string> {
  const ref = crypto.randomUUID();
  const record: CustomImageRecord = { ref, blob };
  await withStore("readwrite", (store) => store.put(record, SLOT));
  return ref;
}

/** Load the uploaded image for `ref`, or `null` if it was overwritten/cleared. */
export async function loadCustomBackground(ref: string): Promise<Blob | null> {
  const record = await withStore<CustomImageRecord | undefined>("readonly", (store) =>
    store.get(SLOT),
  );
  return record && record.ref === ref ? record.blob : null;
}
