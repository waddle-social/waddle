/**
 * Collect the files carried by a paste's `DataTransfer`, of any type.
 *
 * Browsers expose the same pasted file through both `files` and
 * `items` (kind `"file"`), so `files` is preferred and `items` is only
 * consulted when `files` is empty; that fallback is de-duplicated by
 * name, type and size because an engine may list one file twice. Empty
 * files are dropped because the XEP-0363 upload path rejects zero-byte
 * payloads.
 */
export function clipboardFiles(data: DataTransfer): File[] {
  const listed = Array.from(data.files ?? []);
  const candidates = listed.length > 0 ? listed : uniqueFiles(fileItems(data.items));
  return candidates.filter((file) => file.size > 0);
}

function fileItems(items: DataTransferItemList | undefined): File[] {
  return Array.from(items ?? [])
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null);
}

function uniqueFiles(files: readonly File[]): File[] {
  const seen = new Set<string>();
  return files.filter((file) => {
    const key = `${file.name}\u0000${file.type}\u0000${file.size}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}
