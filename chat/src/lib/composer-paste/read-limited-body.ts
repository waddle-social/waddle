/**
 * Read a response body into memory, giving up (and cancelling the
 * stream) as soon as it grows past `maxBytes`.
 */
export async function readLimitedBody(response: Response, maxBytes: number): Promise<Uint8Array<ArrayBuffer> | null> {
  const reader = response.body?.getReader();
  if (!reader) return withinLimit(new Uint8Array(await response.arrayBuffer()), maxBytes);
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) return concatChunks(chunks, total);
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel().catch(() => undefined);
      return null;
    }
    chunks.push(value);
  }
}

function withinLimit(bytes: Uint8Array<ArrayBuffer>, maxBytes: number): Uint8Array<ArrayBuffer> | null {
  return bytes.byteLength <= maxBytes ? bytes : null;
}

function concatChunks(chunks: readonly Uint8Array[], total: number): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}
