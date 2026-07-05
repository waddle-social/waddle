// Scheme allowlist for shared-file attachment URLs (XEP-0447 sources and
// friends). Attachment URLs arrive over the wire (and out of MAM history)
// fully attacker-controlled, so anything bound into `href`/`src` MUST pass
// through here first. Only `https:` remote URLs are allowed: plain `http:`
// is rejected so a "safe" attachment can never be a mixed-content fetch the
// app's CSP would block anyway (the CSP fetch directives are https-only).
// `blob:` is allowed so this guard can also re-validate already-resolved
// local object URLs (e.g. the decrypted-attachment and lightbox paths pass
// a locally-minted `blob:` URL back through it); a remote-supplied `blob:`
// string is harmless since object URLs are origin- and agent-scoped and
// cannot reference another session's blob. Everything else (`http:`,
// `javascript:`, `data:`, `vbscript:`, unparseable garbage) is rejected.
const ALLOWED_ATTACHMENT_PROTOCOLS = new Set(["https:", "blob:"]);

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
