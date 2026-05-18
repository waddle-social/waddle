/**
 * Build a tooltip-friendly keyboard shortcut string from a Mod-key spec.
 *
 * Pass the same spec TipTap uses ("Mod-B", "Mod-Shift-X") and get back a
 * platform-aware label ("⌘B" on macOS, "Ctrl+B" elsewhere).
 *
 * Detection prefers the modern `navigator.userAgentData.platform`; the legacy
 * `navigator.platform` and a userAgent sniff cover older browsers.
 */
export function formatModShortcut(spec: string, ua: NavigatorLike = globalThis.navigator): string {
  const mac = isMacPlatform(ua);
  const parts = spec.split("-").map((part) => {
    if (part === "Mod") return mac ? "⌘" : "Ctrl";
    if (part === "Shift") return mac ? "⇧" : "Shift";
    if (part === "Alt") return mac ? "⌥" : "Alt";
    if (part === "Ctrl") return mac ? "⌃" : "Ctrl";
    return part;
  });
  return mac ? parts.join("") : parts.join("+");
}

export interface NavigatorLike {
  readonly platform?: string;
  readonly userAgent?: string;
  readonly userAgentData?: { readonly platform?: string };
}

export function isMacPlatform(ua: NavigatorLike | undefined): boolean {
  if (!ua) return false;
  const fromData = ua.userAgentData?.platform;
  if (fromData) return /mac/i.test(fromData);
  if (ua.platform && /mac|iphone|ipad|ipod/i.test(ua.platform)) return true;
  if (ua.userAgent && /Mac|iPhone|iPad|iPod/i.test(ua.userAgent)) return true;
  return false;
}
