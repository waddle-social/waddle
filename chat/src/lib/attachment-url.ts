// Scheme allowlist for shared-file attachment URLs (XEP-0447 sources and
// friends). Attachment URLs arrive over the wire (and out of MAM history)
// fully attacker-controlled, so anything bound into `href`/`src` MUST pass
// through here first. `blob:` is allowed because decrypted attachments are
// locally-minted object URLs; everything else (`javascript:`, `data:`,
// `vbscript:`, unparseable garbage) is rejected.
const ALLOWED_ATTACHMENT_PROTOCOLS = new Set(["http:", "https:", "blob:"]);

export function safeAttachmentUrl(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  if (!trimmed) return null;
  try {
    const url = new URL(trimmed);
    return ALLOWED_ATTACHMENT_PROTOCOLS.has(url.protocol) ? trimmed : null;
  } catch {
    return null;
  }
}
